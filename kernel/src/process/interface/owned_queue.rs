use core::ptr::NonNull;

use kapi::queue::{queue_size_in_bytes, Queue, QueueId};

use crate::{
    assert_send,
    memory::{paging::PageTableEntryOptions, MemoryError, PhysicalBuffer, PAGE_SIZE},
};

/// A single synchronized queue which owns its backing memory.
///
/// Works the same as an NVMe queue.
/// A queue is internally mutable and synchronized.
pub struct OwnedQueue<T> {
    pub buffer: PhysicalBuffer,
    queue: Queue<T>,
}

impl<T> OwnedQueue<T> {
    /// Allocate and Initialize a new queue backed by memory.
    pub fn new(id: QueueId, capacity: usize) -> Result<OwnedQueue<T>, MemoryError> {
        let size_in_pages = queue_size_in_bytes::<T>(capacity).div_ceil(PAGE_SIZE);

        let buffer = PhysicalBuffer::alloc(
            size_in_pages,
            &PageTableEntryOptions {
                read_only: false,
                el0_access: true,
            },
        )?;

        let queue = unsafe {
            Queue::new(
                id,
                NonNull::new(buffer.virtual_address().as_ptr()).expect("buffer non-null"),
                capacity,
            )
        };

        // make sure that the queue is set up correctly.
        unsafe {
            queue.initialize();
        }

        Ok(Self { buffer, queue })
    }
}

impl<T> core::ops::Deref for OwnedQueue<T> {
    type Target = Queue<T>;

    fn deref(&self) -> &Self::Target {
        &self.queue
    }
}

impl<T> core::fmt::Debug for OwnedQueue<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OwnedQueue")
            .field("buffer", &self.buffer)
            .field("id", &self.queue.id)
            .field("capacity", &self.queue.capacity())
            .finish()
    }
}

assert_send!(kapi::commands::Command);
assert_send!(kapi::completions::Completion);

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn basic() {
        let qu = OwnedQueue::new(QueueId::new(1).unwrap(), 16).expect("create queue");
        assert_eq!(qu.poll(), None);
        qu.post(5).unwrap();
        qu.post(6).unwrap();
        qu.post(7).unwrap();
        assert_eq!(qu.poll(), Some(5));
        assert_eq!(qu.poll(), Some(6));
        assert_eq!(qu.poll(), Some(7));
    }
}
