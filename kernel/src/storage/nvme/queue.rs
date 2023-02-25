use alloc::sync::Arc;
use hashbrown::HashMap;
use spin::Mutex;

use super::*;
use crate::memory::PhysicalAddress;

pub struct Command<'sq> {
    parent: &'sq mut SubmissionQueue,
    new_tail: u16,
    ptr: *mut u8,
}

impl<'sq> Command<'sq> {
    pub fn submit(self) {
        self.parent.tail = self.new_tail;
        unsafe {
            self.parent.tail_doorbell.write_volatile(self.parent.tail);
        }
    }
}

pub type QueueId = u16;

pub struct SubmissionQueue {
    id: QueueId,
    base_address: PhysicalAddress,
    entry_count: u16,
    base_ptr: *mut u8,
    tail: u16,
    tail_doorbell: *mut u16,
    head: Arc<Mutex<u16>>,
}

impl SubmissionQueue {
    pub fn full(&self) -> bool {
        *self.head.lock() == self.tail + 1
    }

    pub fn begin(&mut self) -> Option<Command> {
        if self.full() {
            None
        } else {
            let ptr = unsafe {
                self.base_ptr
                    .offset(((self.tail as usize) * SUBMISSION_ENTRY_SIZE) as isize)
            };
            Some(Command {
                ptr,
                new_tail: (self.tail + 1) % self.entry_count,
                parent: self,
            })
        }
    }
}

bitfield! {
    pub struct CompletionStatus(u16);
    u8;
    do_not_retry, _: 15;
    more, _: 14;
    command_retry_delay, _: 13, 12;
    status_code_type, _: 11, 9;
    status_code, _: 8, 1;
    phase_tag, _: 0;
}

impl Clone for CompletionStatus {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

#[repr(C)]
#[derive(Clone)]
pub struct Completion {
    cmd: u32,
    res: u32,
    sqid: QueueId,
    head: u16,
    status: CompletionStatus,
    id: u16,
}

pub struct CompletionQueue {
    id: QueueId,
    base_address: PhysicalAddress,
    entry_count: u16,
    base_ptr: *mut u8,
    head: u16,
    head_doorbell: *mut u16,
    current_phase: bool,
    assoc_submit_queue_heads: HashMap<QueueId, Arc<Mutex<u16>>>,
}

impl CompletionQueue {
    pub fn pop(&mut self) -> Option<Completion> {
        let cmp = unsafe {
            let ptr = self
                .base_ptr
                .offset((self.head as usize * COMPLETION_ENTRY_SIZE) as isize)
                as *mut Completion;

            ptr.as_ref().unwrap()
        };
        if cmp.status.phase_tag() == self.current_phase {
            self.head += 1;
            if self.head > self.entry_count {
                self.head = 0;
                self.current_phase = !self.current_phase;
            }
            *self
                .assoc_submit_queue_heads
                .get(&cmp.sqid)
                .expect("completion queue has head reference for all associated submission queues")
                .lock() = cmp.head;
            unsafe {
                self.head_doorbell.write_volatile(self.head);
            }
            Some(cmp.clone())
        } else {
            None
        }
    }

    pub fn associate(&mut self, submit_queue: &SubmissionQueue) {
        self.assoc_submit_queue_heads
            .insert(submit_queue.id, submit_queue.head.clone());
    }

    // TODO: could easily implement peek()
}
