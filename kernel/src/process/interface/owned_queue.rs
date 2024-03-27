use core::ptr::NonNull;

use bytemuck::Zeroable;
use kapi::queue::{queue_size_in_bytes, Queue, QueueId};

use crate::memory::{paging::PageTableEntryOptions, MemoryError, PhysicalBuffer, PAGE_SIZE};

/// A single synchronized queue which owns its backing memory.
///
/// Works the same as an NVMe queue.
/// A queue is internally mutable and synchronized.
pub struct OwnedQueue<T> {
    pub buffer: PhysicalBuffer,
    queue: Queue<T>,
}

impl<T: Zeroable> OwnedQueue<T> {
    /// Allocate a new queue backed by memory.
    ///
    /// `T` must be [Zeroable] for this operation to be safe, since the queue memory is
    /// initially zeroed.
    pub fn new(id: QueueId, queue_len: usize) -> Result<OwnedQueue<T>, MemoryError> {
        let size_in_pages = queue_size_in_bytes::<T>(queue_len).div_ceil(PAGE_SIZE);

        let buffer = PhysicalBuffer::alloc_zeroed(
            size_in_pages,
            &PageTableEntryOptions {
                read_only: false,
                el0_access: true,
            },
        )?;

        let queue = unsafe {
            Queue::new(
                id,
                queue_len,
                NonNull::new(buffer.virtual_address().as_ptr()).expect("buffer non-null"),
            )
        };

        Ok(Self { buffer, queue })
    }
}

impl<T> core::ops::Deref for OwnedQueue<T> {
    type Target = Queue<T>;

    fn deref(&self) -> &Self::Target {
        &self.queue
    }
}
