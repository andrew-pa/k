use core::sync::atomic::AtomicUsize;

use crate::memory::{paging::PageTableEntryOptions, MemoryError, PhysicalBuffer};

use kapi::Message;

pub type QueueId = u32;

struct Queue {
    id: QueueId,
    buffer: PhysicalBuffer,
    head_ptr: *mut AtomicUsize,
    tail_ptr: *mut AtomicUsize,
}

impl Queue {
    fn new(id: QueueId, size_in_pages: usize) -> Result<Queue, MemoryError> {
        let mut buffer =
            PhysicalBuffer::alloc(
                size_in_pages,
                &PageTableEntryOptions {
                    read_only: false,
                    el0_access: true,
                },
            )?;

        // zero the queue contents
        buffer.as_bytes_mut().fill(0);

        // place the head/tail pointers at the end of the buffer
        let p = buffer
            .virtual_address()
            .offset((buffer.len() - 2 * core::mem::size_of::<usize>()) as isize)
            .as_ptr();

        Ok(Queue {
            id,
            head_ptr: p,
            tail_ptr: unsafe { p.offset(1) },
            buffer,
        })
    }
}

/// A [SubmissionQueue] queues messages from a process to the kernel. The user-space process is the "submitter".
pub struct SubmissionQueue(Queue);

impl SubmissionQueue {
    /// Create a new submission queue.
    pub fn new(id: QueueId, size_in_pages: usize) -> Result<Queue, MemoryError> {
        Queue::new(id, size_in_pages)
    }

    /// Returns the first outstanding message in the queue if present.
    pub fn poll(&mut self) -> Option<Message> {
        todo!()
    }
}

/// A [CompletionQueue] queues messages from the kernel to the a user-space process. These messages represent the completion of actions requested by previous messages submitted by the process.
pub struct CompletionQueue(Queue);

impl CompletionQueue {
    /// Create a new completion queue.
    pub fn new(id: QueueId, size_in_pages: usize) -> Result<Queue, MemoryError> {
        Queue::new(id, size_in_pages)
    }

    /// Post a completion message on the queue.
    pub fn post(&mut self, message: Message) {
        todo!()
    }
}
