use core::{ptr::NonNull, sync::atomic::AtomicUsize};

use crate::memory::{paging::PageTableEntryOptions, MemoryError, PhysicalBuffer};

use kapi::{Command, Completion};
use postcard::experimental::max_size::MaxSize;
use snafu::Snafu;

#[derive(Debug, Snafu)]
pub enum QueueError {
    #[snafu(display("queue is too full to receive message"))]
    Full,
    Serialize {
        cause: postcard::Error,
    },
}

pub type QueueId = u32;

/// A single synchronized queue. Works the same as an NVMe queue.
struct Queue<T> {
    buffer: PhysicalBuffer,
    queue_len: usize,
    /// A pointer into the buffer to the value of the head index.
    head_ptr: NonNull<AtomicUsize>,
    /// A pointer into the buffer to the value of the tail index.
    tail_ptr: NonNull<AtomicUsize>,
    /// A pointer into the buffer to the start of the actual data in the queue.
    data_ptr: NonNull<T>, // TODO: we might need an additional lock to synchronize access to queues between
                          // different threads in the kernel?
}

impl<T> Queue<T> {
    fn new(size_in_pages: usize) -> Result<Queue<T>, MemoryError> {
        let mut buffer = PhysicalBuffer::alloc(
            size_in_pages,
            &PageTableEntryOptions {
                read_only: false,
                el0_access: true,
            },
        )?;

        // zero the queue contents
        buffer.as_bytes_mut().fill(0);

        // compute the number of slots in the queue
        let queue_len =
            (buffer.len() - (core::mem::size_of::<AtomicUsize>() * 2)) / core::mem::size_of::<T>();

        // place the head/tail pointers at the start of the buffer
        let p: *mut AtomicUsize = buffer.virtual_address().as_ptr();

        // put the messages after the head and tail pointers in the buffer
        let data_ptr: *mut T = buffer
            .virtual_address()
            .offset((core::mem::size_of::<AtomicUsize>() * 2) as isize)
            .as_ptr();

        Ok(Queue {
            queue_len,
            head_ptr: NonNull::new(p).unwrap(),
            tail_ptr: NonNull::new(unsafe { p.offset(1) }).unwrap(),
            data_ptr: NonNull::new(data_ptr).unwrap(),
            buffer,
        })
    }

    /// Gets the head index (the index of the next free slot).
    #[inline]
    fn head(&self) -> usize {
        // TODO: we need to check to make sure that the user space process hasn't messed this value
        // up and that it is still in bounds
        unsafe {
            self.head_ptr
                .as_ref()
                .load(core::sync::atomic::Ordering::Acquire)
        }
    }

    /// Move the head forward one slot.
    #[inline]
    fn move_head(&mut self) {
        unsafe {
            let head = self.head_ptr.as_mut();
            if head.fetch_add(1, core::sync::atomic::Ordering::AcqRel) == self.queue_len - 1 {
                head.store(0, core::sync::atomic::Ordering::Release);
            }
        }
    }

    /// Gets the tail index (the index of the next pending message).
    #[inline]
    fn tail(&self) -> usize {
        // TODO: we need to check to make sure that the user space process hasn't messed this value
        // up and that it is still in bounds
        unsafe {
            self.tail_ptr
                .as_ref()
                .load(core::sync::atomic::Ordering::Acquire)
        }
    }

    /// Move the tail forward one slot.
    #[inline]
    fn move_tail(&mut self) {
        unsafe {
            let tail = self.tail_ptr.as_mut();
            if tail.fetch_add(1, core::sync::atomic::Ordering::AcqRel) == self.queue_len - 1 {
                tail.store(0, core::sync::atomic::Ordering::Release);
            }
        }
    }

    /// Returns the first outstanding message in the queue if present.
    pub fn poll(&mut self) -> Option<T> {
        let head = self.head();
        (head != self.tail()).then(|| unsafe {
            log::trace!("head = {head}, tail = {}", self.tail());
            let cmd = self.data_ptr.offset(head as isize).read();
            self.move_head();
            cmd
        })
    }

    /// Post a message in the queue.
    pub fn post(&mut self, msg: &T) -> Result<(), QueueError> {
        let tail = self.tail();
        (self.head() != (tail + 1) % self.queue_len)
            .then(|| {
                unsafe {
                    let dst = self.data_ptr.offset(tail as isize);
                    core::ptr::copy(msg, dst.as_ptr(), 1);
                }
                self.move_tail();
            })
            .ok_or(QueueError::Full)
    }
}

/// A bi-directional communication channel between the kernel and a user-space process using shared memory.
pub struct Channel {
    /// The submission queue queues messages from a process to the kernel. The user-space process is the "submitter".
    submission: Queue<Command>,

    /// A completion queue queues messages from the kernel to the a user-space process. These messages represent the completion of actions requested by previous messages submitted by the process.
    completion: Queue<Completion>,
}

impl Channel {
    /// Allocate a new channel.
    pub fn new(
        submission_size_in_pages: usize,
        completion_size_in_pages: usize,
    ) -> Result<Channel, MemoryError> {
        Ok(Channel {
            submission: Queue::new(submission_size_in_pages)?,
            completion: Queue::new(completion_size_in_pages)?,
        })
    }

    pub fn submission_queue_buffer(&self) -> &PhysicalBuffer {
        &self.submission.buffer
    }

    pub fn completion_queue_buffer(&self) -> &PhysicalBuffer {
        &self.completion.buffer
    }

    /// Returns the first outstanding command message in the channel if present.
    pub fn poll(&mut self) -> Option<Command> {
        self.submission.poll()
    }

    /// Post a completion message in the channel.
    pub fn post(&mut self, msg: &Completion) -> Result<(), QueueError> {
        self.completion.post(msg)
    }
}
