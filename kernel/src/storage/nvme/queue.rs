use alloc::sync::Arc;
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
            true,
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
