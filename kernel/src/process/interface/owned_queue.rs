use core::ptr::NonNull;

use kapi::queue::{Queue, QueueId};

use crate::memory::{paging::PageTableEntryOptions, MemoryError, PhysicalBuffer};

/// A single synchronized queue which owns its backing memory.
///
/// Works the same as an NVMe queue.
/// A queue is internally mutable and synchronized.
pub struct OwnedQueue<T> {
    buffer: PhysicalBuffer,
    queue: Queue<T>,
}

impl<T> OwnedQueue<T> {
    pub fn new(id: QueueId, size_in_pages: usize) -> Result<OwnedQueue<T>, MemoryError> {
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
                buffer.len(),
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
