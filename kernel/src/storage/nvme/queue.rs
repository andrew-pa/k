use super::{command::QueuePriority, *};
use bitfield::BitRangeMut;
use bytemuck::{bytes_of, Pod, Zeroable};

use hashbrown::HashMap;
use snafu::{ensure, Snafu};

use crate::memory::{paging::PageTableEntryOptions, MemoryError, PhysicalAddress};

// TODO: detect if the NVMe device has SGL and use it instead. Alas, QEMU does not appear to
// support it, so for now the PRP list will have to do.
#[repr(C)]
#[allow(unused)]
#[derive(Pod, Zeroable, Clone, Copy, Debug)]
struct ScatterGatherDescriptor([u8; 16]);

#[allow(unused)]
impl ScatterGatherDescriptor {
    #[inline]
    fn typical(r#type: u8, address: u64, length: u64) -> ScatterGatherDescriptor {
        let mut d = [0u8; 16];
        d[0..8].copy_from_slice(bytes_of(&address));
        d[8..12].copy_from_slice(bytes_of(&length));
        d[15] = r#type << 4;
        ScatterGatherDescriptor(d)
    }

    pub(super) fn data_block(address: u64, length: u64) -> ScatterGatherDescriptor {
        ScatterGatherDescriptor::typical(0, address, length)
    }

    pub(super) fn next_segment(address: u64, length: u64) -> ScatterGatherDescriptor {
        ScatterGatherDescriptor::typical(2, address, length)
    }

    pub(super) fn next_segment_final(address: u64, length: u64) -> ScatterGatherDescriptor {
        ScatterGatherDescriptor::typical(3, address, length)
    }
}

pub struct Command<'sq> {
    parent: &'sq mut SubmissionQueue,
    new_tail: u16,
    cmd: &'sq mut [u32],
    extra_data_ptr_buffers: SmallVec<[PhysicalBuffer; 1]>,
}

impl core::fmt::Debug for Command<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut x = f.debug_map();
        x.entry(&"parent-queue", &self.parent.id);
        for (i, w) in self.cmd.iter().enumerate() {
            x.entry(&format_args!("dw{i}"), &format_args!("0x{:x}", w));
        }
        x.finish()
    }
}

impl<'sq> Command<'sq> {
    pub fn submit(self) -> SmallVec<[PhysicalBuffer; 1]> {
        log::trace!("submitting NVMe command {self:?}");
        self.parent.tail = self.new_tail;
        unsafe {
            self.parent.tail_doorbell.write_volatile(self.parent.tail);
        }
        log::trace!("submitted!");
        self.extra_data_ptr_buffers
    }

    // low-level accessors

    #[inline]
    pub fn set_command_id(self, id: u16) -> Command<'sq> {
        log::trace!("setting command id to {id}");
        self.cmd[0].set_bit_range(31, 16, id);
        self
    }

    #[inline]
    pub fn set_opcode(self, op: u8) -> Command<'sq> {
        self.cmd[0].set_bit_range(7, 0, op);
        self
    }

    #[inline]
    pub fn set_namespace_id(self, id: u32) -> Command<'sq> {
        self.cmd[1] = id;
        self
    }

    #[inline]
    pub fn set_data_ptr_single(self, ptr: PhysicalAddress) -> Command<'sq> {
        // set data ptr to PRP mode
        // self.cmd[0].set_bit_range(15, 14, 0);

        self.set_qword(6, ptr.0 as u64)
    }

    /// Sets the NVMe command data pointer to point to the regions specified in the format
    /// given by [crate::storage::BlockStore]. Assumes the vector is valid.
    pub fn set_data_ptrs(
        mut self,
        regions: &[(PhysicalAddress, usize)],
        blocks_per_page: usize,
    ) -> Result<Command<'sq>, crate::storage::StorageError> {
        ensure!(
            !regions.is_empty(),
            crate::storage::BadVectorSnafu {
                reason: "must have at least one entry",
                entry: None
            }
        );

        // the four dwords that make up the "Data Pointer" field in the command form the first
        // PRP "page", although there is only space for two entries.
        let mut prp_page: &mut [PhysicalAddress] = bytemuck::cast_slice_mut(&mut self.cmd[6..10]);
        let mut current_offset = 0;

        for region in regions {
            let mut num_blocks = 0;
            let mut next_page_addr = region.0;
            while num_blocks < region.1 {
                if current_offset == prp_page.len() - 1 {
                    let buf = PhysicalBuffer::alloc_zeroed(1, &Default::default())
                        .context(crate::storage::MemorySnafu)?;
                    prp_page[current_offset] = buf.physical_address();
                    current_offset = 0;
                    self.extra_data_ptr_buffers.push(buf);
                    prp_page = bytemuck::cast_slice_mut(
                        self.extra_data_ptr_buffers
                            .last_mut()
                            .unwrap()
                            .as_bytes_mut(),
                    );
                }

                log::trace!("region = {region:?}, current_offset = {current_offset}, num_blocks={num_blocks}, prp_page.len() = {}", prp_page.len());
                prp_page[current_offset] = next_page_addr;
                next_page_addr = next_page_addr.add(PAGE_SIZE);
                current_offset += 1;
                num_blocks += blocks_per_page;
            }
        }

        Ok(self)
    }

    /// Set a qword (64-bit) value in the command. Index is in terms of dwords.
    #[inline]
    pub fn set_qword(self, idx: usize, data: u64) -> Command<'sq> {
        self.cmd[idx] = data as u32;
        self.cmd[idx + 1] = (data >> 32) as u32;
        self
    }

    #[inline]
    pub fn set_dword(self, idx: usize, data: u32) -> Command<'sq> {
        self.cmd[idx] = data;
        self
    }
}

pub type QueueId = u16;

#[derive(Debug, Snafu)]
pub enum QueueCreateError {
    Generic { code: u8 },
    InvalidQueueId,
    InvalidQueueSize,
    InvalidInterruptVectorIndex,
    InvalidCompletionQueueId,
}

pub struct SubmissionQueue {
    id: QueueId,
    queue_memory: PhysicalBuffer,
    entry_count: u16,
    tail: u16,
    tail_doorbell: *mut u16,
    head: Arc<Mutex<u16>>,
    parent: Option<Arc<Mutex<SubmissionQueue>>>,
}

impl SubmissionQueue {
    fn new(
        id: QueueId,
        entry_count: u16,
        doorbell_base: VirtualAddress,
        doorbell_stride: u8,
        associated_completion_queue: &mut CompletionQueue,
        parent: Option<Arc<Mutex<SubmissionQueue>>>,
    ) -> Result<SubmissionQueue, MemoryError> {
        let tail_doorbell =
            doorbell_base.add((2 * (id as usize)) * (4 << doorbell_stride as usize));
        let head: Arc<Mutex<u16>> = Arc::default();
        associated_completion_queue
            .assoc_submit_queue_heads
            .insert(id, head.clone());
        let page_count = (entry_count as usize * SUBMISSION_ENTRY_SIZE).div_ceil(PAGE_SIZE);
        Ok(SubmissionQueue {
            id,
            queue_memory: PhysicalBuffer::alloc_zeroed(
                page_count,
                &PageTableEntryOptions::default(),
            )?,
            entry_count,
            tail: 0,
            tail_doorbell: tail_doorbell.as_ptr(),
            head,
            parent,
        })
    }

    pub(super) fn new_admin(
        entry_count: u16,
        doorbell_base: VirtualAddress,
        doorbell_stride: u8,
        associated_completion_queue: &mut CompletionQueue,
    ) -> Result<Self, MemoryError> {
        Self::new(
            0,
            entry_count,
            doorbell_base,
            doorbell_stride,
            associated_completion_queue,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn new_io(
        id: QueueId,
        entry_count: u16,
        doorbell_base: VirtualAddress,
        doorbell_stride: u8,
        associated_completion_queue: &mut CompletionQueue,
        parent: Arc<Mutex<SubmissionQueue>>,
        parent_cq: &mut CompletionQueue,
        priority: QueuePriority,
    ) -> Result<Self, Error> {
        assert!(id > 0);
        let s = Self::new(
            id,
            entry_count,
            doorbell_base,
            doorbell_stride,
            associated_completion_queue,
            Some(parent.clone()),
        )
        .context(error::MemorySnafu {
            reason: "allocate backing memory for NVMe submission queue",
        })?;
        // TODO: for now all queues are physically continuous
        parent
            .lock()
            .begin()
            .expect("admin queue full")
            .create_io_submission_queue(
                id,
                s.size(),
                associated_completion_queue.id,
                priority,
                true,
            )
            .set_data_ptr_single(s.address())
            .submit();
        let cmpl = parent_cq.busy_wait_for_completion();
        log::trace!("create IO SubmissionQueue completion={cmpl:?}");
        match (cmpl.status.status_code_type(), cmpl.status.status_code()) {
            (0, 0) => Ok(s),
            (0, code) => Err(QueueCreateError::Generic { code }),
            (1, 1) => Err(QueueCreateError::InvalidCompletionQueueId),
            (1, 2) => Err(QueueCreateError::InvalidQueueId),
            (1, 8) => Err(QueueCreateError::InvalidQueueSize),
            (_, _) => panic!("unexpected IO submission queue completion: {cmpl:?}"),
        }
        .map_err(|e| Box::new(e) as _)
        .context(error::OtherSnafu {
            reason: "NVMe device failed to create queue",
            code: None,
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
                        .add((self.tail as usize) * SUBMISSION_ENTRY_SIZE)
                        .as_ptr::<u32>(),
                    16,
                )
            };
            cmd.fill(0);
            Some(Command {
                cmd,
                new_tail: (self.tail + 1) % self.entry_count,
                parent: self,
                extra_data_ptr_buffers: SmallVec::new(),
            })
        }
    }
}

unsafe impl Send for SubmissionQueue {}

// TODO: WARN: if these queues get dropped while they are still active, the NVMe driver may try to
// write to them, which will corrupt memory if reallocated elsewhere.
// Ideally an IO queue would automatically send a destroy command on the admin queue when dropped
// but, what about the admin queues themselves?
impl Drop for SubmissionQueue {
    fn drop(&mut self) {
        let mut parent = self
            .parent
            .as_ref()
            .expect("dropping an NVMe queue w/o informing the driver is unsafe")
            .lock();
        parent
            .begin()
            .expect("admin queue has space")
            .delete_io_submission_queue(self.id)
            .submit();
    }
}

bitfield! {
    pub struct CompletionStatus(u16);
    impl Debug;
    u8;
    pub do_not_retry, _: 15;
    pub more, _: 14;
    pub command_retry_delay, _: 13, 12;
    pub status_code_type, _: 11, 9;
    pub status_code, _: 8, 1;
    phase_tag, _: 0;
}

impl Clone for CompletionStatus {
    fn clone(&self) -> Self {
        Self(self.0)
    }
}

#[repr(C)]
#[derive(Clone, Debug)]
pub struct Completion {
    /// command specfic field
    pub cmd: u32,
    /// reserved field
    pub res: u32,
    /// new head pointer for submission queue `sqid`
    pub head: u16,
    // ID of the submission queue that submitted the command
    pub sqid: QueueId,
    /// command identifier specified by host
    pub id: u16,
    pub status: CompletionStatus,
}

pub struct CompletionQueue {
    id: QueueId,
    queue_memory: PhysicalBuffer,
    entry_count: u16,
    head: u16,
    head_doorbell: *mut u16,
    current_phase: bool,
    assoc_submit_queue_heads: HashMap<QueueId, Arc<Mutex<u16>>>,
    parent: Option<Arc<Mutex<SubmissionQueue>>>,
}

impl CompletionQueue {
    fn new(
        id: QueueId,
        entry_count: u16,
        doorbell_base: VirtualAddress,
        doorbell_stride: u8,
        parent: Option<Arc<Mutex<SubmissionQueue>>>,
    ) -> Result<CompletionQueue, MemoryError> {
        let head_doorbell =
            doorbell_base.add((2 * (id as usize) + 1) * (4 << doorbell_stride as usize));
        let page_count = (entry_count as usize * COMPLETION_ENTRY_SIZE).div_ceil(PAGE_SIZE);
        Ok(CompletionQueue {
            id,
            queue_memory: PhysicalBuffer::alloc_zeroed(
                page_count,
                &PageTableEntryOptions::default(),
            )?,
            entry_count,
            head: 0,
            head_doorbell: head_doorbell.as_ptr(),
            current_phase: true,
            assoc_submit_queue_heads: HashMap::new(),
            parent,
        })
    }

    pub(super) fn new_admin(
        entry_count: u16,
        doorbell_base: VirtualAddress,
        doorbell_stride: u8,
    ) -> Result<Self, MemoryError> {
        Self::new(0, entry_count, doorbell_base, doorbell_stride, None)
    }

    pub(super) fn new_io(
        id: QueueId,
        entry_count: u16,
        doorbell_base: VirtualAddress,
        doorbell_stride: u8,
        parent: Arc<Mutex<SubmissionQueue>>,
        parent_cq: &mut CompletionQueue,
        interrupt_index: Option<u16>,
    ) -> Result<Self, Error> {
        assert!(id > 0);
        let s = Self::new(
            id,
            entry_count,
            doorbell_base,
            doorbell_stride,
            Some(parent.clone()),
        )
        .context(error::MemorySnafu {
            reason: "failed to allocate memory for NVMe completion queue",
        })?;
        // TODO: for now all queues are physically continuous
        parent
            .lock()
            .begin()
            .expect("admin queue full")
            .create_io_completion_queue(
                id,
                s.size(),
                interrupt_index.unwrap_or(0),
                interrupt_index.is_some(),
                true,
            )
            .set_data_ptr_single(s.address())
            .submit();
        let cmpl = parent_cq.busy_wait_for_completion();
        log::trace!("create IO CompletionQueue completion={cmpl:?}");
        match (cmpl.status.status_code_type(), cmpl.status.status_code()) {
            (0, 0) => Ok(s),
            (0, code) => Err(QueueCreateError::Generic { code }),
            (1, 1) => Err(QueueCreateError::InvalidQueueId),
            (1, 2) => Err(QueueCreateError::InvalidQueueSize),
            (1, 8) => Err(QueueCreateError::InvalidInterruptVectorIndex),
            (_, _) => panic!("unexpected IO submission queue completion: {cmpl:?}"),
        }
        .map_err(|e| Box::new(e) as _)
        .context(error::OtherSnafu {
            reason: "NVMe device failed to create queue",
            code: None,
        })
    }

    pub fn queue_id(&self) -> QueueId {
        self.id
    }

    pub fn size(&self) -> u16 {
        self.entry_count
    }

    pub fn address(&self) -> PhysicalAddress {
        self.queue_memory.physical_address()
    }

    pub fn pop(&mut self) -> Option<Completion> {
        let cmp = unsafe {
            // let sl = (self
            //     .queue_memory
            //     .virtual_address()
            //     .add(self.head as usize * COMPLETION_ENTRY_SIZE)
            //     .as_ptr::<[u32; 4]>())
            // .read_volatile();
            // log::trace!(
            //     "cmp raw:\n0: {:8x}\n1: {:8x}\n2: {:8x}\n3: {:8x}",
            //     sl[0],
            //     sl[1],
            //     sl[2],
            //     sl[3]
            // );

            let ptr = self
                .queue_memory
                .virtual_address()
                .add(self.head as usize * COMPLETION_ENTRY_SIZE)
                .as_ptr::<Completion>();

            ptr.as_ref().unwrap()
        };
        log::trace!("read cmp: {cmp:?}, head = {}", self.head);
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

unsafe impl Send for CompletionQueue {}

// TODO: WARN: see drop impl for SubmissionQueue
impl Drop for CompletionQueue {
    fn drop(&mut self) {
        // TODO: we need to make sure all associated submission queues are dropped before
        // we drop the completion queue
        let mut parent = self
            .parent
            .as_ref()
            .expect("dropping an NVMe queue w/o informing the driver is unsafe")
            .lock();
        parent
            .begin()
            .expect("admin queue has space")
            .delete_io_completion_queue(self.id)
            .submit();
    }
}
