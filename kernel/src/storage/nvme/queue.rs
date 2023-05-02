use alloc::sync::Arc;
use bitfield::BitRangeMut;
use hashbrown::HashMap;
use spin::Mutex;

use super::*;
use crate::memory::{paging::PageTableEntryOptions, MemoryError, PhysicalAddress, PhysicalBuffer};

pub struct Command<'sq> {
    parent: &'sq mut SubmissionQueue,
    new_tail: u16,
    pub cmd: &'sq mut [u32],
}

impl<'sq> Command<'sq> {
    pub fn submit(self) {
        self.parent.tail = self.new_tail;
        unsafe {
            self.parent.tail_doorbell.write_volatile(self.parent.tail);
        }
    }

    // low-level accessors

    pub fn set_command_id(mut self, id: u16) -> Command<'sq> {
        self.cmd[0].set_bit_range(31, 16, id);
        self
    }

    pub fn set_opcode(mut self, op: u8) -> Command<'sq> {
        self.cmd[0].set_bit_range(7, 0, op);
        self
    }

    pub fn set_namespace_id(mut self, id: u32) -> Command<'sq> {
        self.cmd[1] = id;
        self
    }

    pub fn set_metadata_ptr(mut self, ptr: u64) -> Command<'sq> {
        self.cmd[4] = ptr as u32;
        self.cmd[5] = (ptr >> 32) as u32;
        self
    }

    pub fn set_data_ptr_single(mut self, ptr: PhysicalAddress) -> Command<'sq> {
        self.cmd[6] = ptr.0 as u32;
        self.cmd[7] = (ptr.0 >> 32) as u32;
        self
    }

    pub fn set_dword(mut self, idx: usize, data: u32) -> Command<'sq> {
        self.cmd[idx] = data;
        self
    }
}

pub type QueueId = u16;

pub struct SubmissionQueue {
    id: QueueId,
    queue_memory: PhysicalBuffer,
    entry_count: u16,
    tail: u16,
    tail_doorbell: *mut u16,
    head: Arc<Mutex<u16>>,
}

impl SubmissionQueue {
    pub(super) fn new(
        id: QueueId,
        entry_count: u16,
        doorbell_base: VirtualAddress,
        doorbell_stride: u8,
        associated_completion_queue: &mut CompletionQueue,
    ) -> Result<SubmissionQueue, MemoryError> {
        let tail_doorbell =
            doorbell_base.offset((2 * (id as isize)) * (4 << doorbell_stride as isize));
        let head: Arc<Mutex<u16>> = Arc::default();
        associated_completion_queue
            .assoc_submit_queue_heads
            .insert(id, head.clone());
        let page_count = (entry_count as usize * SUBMISSION_ENTRY_SIZE).div_ceil(PAGE_SIZE);
        Ok(SubmissionQueue {
            id,
            queue_memory: PhysicalBuffer::alloc(page_count, &PageTableEntryOptions::default())?,
            entry_count,
            tail: 0,
            tail_doorbell: tail_doorbell.as_ptr(),
            head,
        })
    }

    pub fn size(&self) -> u16 {
        self.entry_count
    }

    pub fn address(&self) -> PhysicalAddress {
        self.queue_memory.physical_address()
    }

    pub fn full(&self) -> bool {
        *self.head.lock() == self.tail + 1
    }

    /// Start a new command in the queue. Returns None if the queue is already full
    pub fn begin(&mut self) -> Option<Command> {
        if self.full() {
            None
        } else {
            let cmd = unsafe {
                core::slice::from_raw_parts_mut(
                    self.queue_memory
                        .virtual_address()
                        .offset(((self.tail as usize) * SUBMISSION_ENTRY_SIZE) as isize)
                        .as_ptr::<u32>(),
                    16,
                )
            };
            cmd.fill(0);
            Some(Command {
                cmd,
                new_tail: (self.tail + 1) % self.entry_count,
                parent: self,
            })
        }
    }
}

// TODO: WARN: if these queues get dropped while they are still active, the NVMe driver may try to
// write to them, which will corrupt memory if reallocated elsewhere.
// Ideally an IO queue would automatically send a destroy command on the admin queue when dropped
// but, what about the admin queues themselves?
impl Drop for SubmissionQueue {
    fn drop(&mut self) {
        todo!("dropping an NVMe queue w/o informing the driver is unsafe")
    }
}

bitfield! {
    pub struct CompletionStatus(u16);
    impl Debug;
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
#[derive(Clone, Debug)]
pub struct Completion {
    /// command specfic field
    cmd: u32,
    /// reserved field
    res: u32,
    /// new head pointer for submission queue `sqid`
    head: u16,
    // ID of the submission queue that submitted the command
    sqid: QueueId,
    /// command identifier specified by host
    id: u16,
    status: CompletionStatus,
}

pub struct CompletionQueue {
    id: QueueId,
    queue_memory: PhysicalBuffer,
    entry_count: u16,
    head: u16,
    head_doorbell: *mut u16,
    current_phase: bool,
    assoc_submit_queue_heads: HashMap<QueueId, Arc<Mutex<u16>>>,
}

impl CompletionQueue {
    pub(super) fn new(
        id: QueueId,
        entry_count: u16,
        doorbell_base: VirtualAddress,
        doorbell_stride: u8,
    ) -> Result<CompletionQueue, MemoryError> {
        let head_doorbell =
            doorbell_base.offset((2 * (id as isize) + 1) * (4 << doorbell_stride as isize));
        let page_count = (entry_count as usize * COMPLETION_ENTRY_SIZE).div_ceil(PAGE_SIZE);
        Ok(CompletionQueue {
            id,
            queue_memory: PhysicalBuffer::alloc(page_count, &PageTableEntryOptions::default())?,
            entry_count,
            head: 0,
            head_doorbell: head_doorbell.as_ptr(),
            current_phase: true,
            assoc_submit_queue_heads: HashMap::new(),
        })
    }

    pub fn size(&self) -> u16 {
        self.entry_count
    }

    pub fn address(&self) -> PhysicalAddress {
        self.queue_memory.physical_address()
    }

    pub fn pop(&mut self) -> Option<Completion> {
        let cmp = unsafe {
            let sl = (self
                .queue_memory
                .virtual_address()
                .offset((self.head as usize * COMPLETION_ENTRY_SIZE) as isize)
                .as_ptr::<[u32; 4]>())
            .read_volatile();
            log::trace!(
                "cmp raw:\n0: {:8x}\n1: {:8x}\n2: {:8x}\n3: {:8x}",
                sl[0],
                sl[1],
                sl[2],
                sl[3]
            );

            let ptr = self
                .queue_memory
                .virtual_address()
                .offset((self.head as usize * COMPLETION_ENTRY_SIZE) as isize)
                .as_ptr::<Completion>();

            ptr.as_ref().unwrap()
        };
        log::trace!("read cmp: {cmp:?}");
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

    pub fn busy_wait_for_completion(&mut self) -> Completion {
        loop {
            if let Some(c) = self.pop() {
                return c;
            }
        }
    }

    // TODO: could easily implement peek()
}

// TODO: WARN: see drop impl for SubmissionQueue
impl Drop for CompletionQueue {
    fn drop(&mut self) {
        todo!("dropping NVMe queue is unsafe w/o informing the driver")
    }
}
