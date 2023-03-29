use alloc::sync::Arc;
use bitfield::{BitRange, BitRangeMut};
use hashbrown::HashMap;
use spin::Mutex;

use super::*;
use crate::memory::{
    paging::{kernel_table, PageTableEntryOptions},
    virtual_address_allocator, MemoryError, PhysicalAddress,
};

fn alloc_queue_memory(
    entry_count: u16,
    size: usize,
) -> Result<(PhysicalAddress, VirtualAddress), MemoryError> {
    let page_count = (entry_count as usize * size).div_ceil(PAGE_SIZE);
    // make sure we know the physical address and also that the pages are allocated
    // continuously in physical memory so we can not bother with queue PRP lists
    let base_address_phy = physical_memory_allocator().alloc_contig(page_count)?;
    let base_address_vir = virtual_address_allocator().alloc(page_count)?;
    kernel_table()
        .map_range(
            base_address_phy,
            base_address_vir,
            page_count,
            true, // TODO: right now this crashes, probably because the overwrite check is broken
            &PageTableEntryOptions::default(),
        )
        .expect("map NVMe queue memory should succeed because VA came from VA allocator");
    Ok((base_address_phy, base_address_vir))
}

fn dealloc_queue_memory(
    entry_count: u16,
    size: usize,
    base_address: PhysicalAddress,
    base_ptr: *mut u8,
) {
    let page_count = (entry_count as usize * size).div_ceil(PAGE_SIZE);
    kernel_table().unmap_range(base_ptr.into(), page_count);
    virtual_address_allocator().free(base_ptr.into(), page_count);
    physical_memory_allocator().free_pages(base_address, page_count);
}

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

    pub fn set_command_id(self, id: u16) -> Command<'sq> {
        self.cmd[0].set_bit_range(31, 16, id);
        self
    }

    pub fn set_opcode(self, op: u8) -> Command<'sq> {
        self.cmd[0].set_bit_range(7, 0, op);
        self
    }

    pub fn set_namespace_id(self, id: u32) -> Command<'sq> {
        self.cmd[1] = id;
        self
    }

    pub fn set_metadata_ptr(self, ptr: u64) -> Command<'sq> {
        self.cmd[4] = ptr as u32;
        self.cmd[5] = (ptr >> 32) as u32;
        self
    }

    pub fn set_data_ptr(self) -> Command<'sq> {
        todo!()
    }

    pub fn set_dword(self, idx: usize, data: u32) -> Command<'sq> {
        self.cmd[idx] = data;
        self
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
        let (base_address_phy, base_address_vir) =
            alloc_queue_memory(entry_count, SUBMISSION_ENTRY_SIZE)?;
        Ok(SubmissionQueue {
            id,
            base_address: base_address_phy,
            entry_count,
            base_ptr: base_address_vir.as_ptr(),
            tail: 0,
            tail_doorbell: tail_doorbell.as_ptr(),
            head,
        })
    }

    pub fn size(&self) -> u16 {
        self.entry_count
    }

    pub fn address(&self) -> PhysicalAddress {
        self.base_address
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
                    self.base_ptr
                        .offset(((self.tail as usize) * SUBMISSION_ENTRY_SIZE) as isize)
                        as *mut u32,
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

impl Drop for SubmissionQueue {
    fn drop(&mut self) {
        dealloc_queue_memory(
            self.entry_count,
            SUBMISSION_ENTRY_SIZE,
            self.base_address,
            self.base_ptr,
        );
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
    pub(super) fn new(
        id: QueueId,
        entry_count: u16,
        doorbell_base: VirtualAddress,
        doorbell_stride: u8,
    ) -> Result<CompletionQueue, MemoryError> {
        let head_doorbell =
            doorbell_base.offset((2 * (id as isize) + 1) * (4 << doorbell_stride as isize));
        let (base_address_phy, base_address_vir) =
            alloc_queue_memory(entry_count, COMPLETION_ENTRY_SIZE)?;
        Ok(CompletionQueue {
            id,
            base_address: base_address_phy,
            entry_count,
            base_ptr: base_address_vir.as_ptr(),
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
        self.base_address
    }

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

    pub fn busy_wait_for_completion(&mut self) -> Completion {
        loop {
            if let Some(c) = self.pop() {
                return c;
            }
        }
    }

    // TODO: could easily implement peek()
}

impl Drop for CompletionQueue {
    fn drop(&mut self) {
        dealloc_queue_memory(
            self.entry_count,
            COMPLETION_ENTRY_SIZE,
            self.base_address,
            self.base_ptr,
        );
    }
}
