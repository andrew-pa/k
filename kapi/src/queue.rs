//! Asynchronous queues for kernel/user-space communication.
use core::{num::NonZeroU16, ptr::NonNull, sync::atomic::AtomicUsize};

use snafu::Snafu;

/// Errors that could occur interacting with a queue.
#[derive(Debug, Snafu)]
pub enum Error {
    /// The queue is full.
    #[snafu(display("queue is too full to receive message"))]
    Full,
}

/// An ID that uniquely identifies a queue relative to the process it is in.
pub type QueueId = NonZeroU16;
/// The ID of the first send queue (created by the kernel) for each process.
pub const FIRST_SEND_QUEUE_ID: QueueId = unsafe { NonZeroU16::new_unchecked(1) };
/// The ID of the first receve queue (created by the kernel) for each process.
pub const FIRST_RECV_QUEUE_ID: QueueId = unsafe { NonZeroU16::new_unchecked(2) };

/// A single synchronized queue reference.
///
/// This queue does *not* own the memory that backs it.
/// Works the same as an NVMe queue.
///
/// A queue is internally mutable and synchronized.
pub struct Queue<T> {
    /// The process-unique ID of this queue.
    pub id: QueueId,
    queue_len: usize,
    /// A pointer into the buffer to the value of the head index.
    head_ptr: NonNull<AtomicUsize>,
    /// A pointer into the buffer to the value of the tail index.
    tail_ptr: NonNull<AtomicUsize>,
    /// A pointer into the buffer to the start of the actual data in the queue.
    data_ptr: NonNull<T>, // TODO: we might need an additional lock to synchronize access to queues between
                          // different threads in the kernel?
}

unsafe impl<T: Send> Send for Queue<T> {}
unsafe impl<T: Send> Sync for Queue<T> {}

impl<T> Queue<T> {
    /// Create a new queue backed by a region of memory.
    ///
    /// # Safety
    /// `backing_memory` must point to a valid region of memory that is at least `size_in_bytes` bytes long.
    /// `backing_memory` must also be correctly aligned for atomic load/store operations of `usize`.
    pub unsafe fn new(id: QueueId, size_in_bytes: usize, backing_memory: NonNull<()>) -> Queue<T> {
        // compute the number of slots in the queue
        let queue_len =
            (size_in_bytes - (core::mem::size_of::<AtomicUsize>() * 2)) / core::mem::size_of::<T>();

        // place the head/tail pointers at the start of the buffer
        let p: NonNull<AtomicUsize> = backing_memory.cast();

        // put the messages after the head and tail pointers in the buffer
        let data_ptr: NonNull<T> = p.add(2).cast();

        Queue {
            id,
            queue_len,
            head_ptr: p,
            tail_ptr: p.add(1),
            data_ptr,
        }
    }

    /// Create a new queue backed by a region of memory from the completion of a queue creation
    /// command. If for some reason the pointer in the completion is null, this function will
    /// panic.
    ///
    /// # Safety
    /// `cmpl.start` must point to a valid region of memory that is at least `cmpl.size_in_bytes` bytes long.
    /// `cmpl.start` must also be correctly aligned for atomic load/store operations of `usize`.
    /// If this completion came from the kernel, it will satisfy these requirements.
    // TODO: it would be excellent if we could check to make sure we really have a queue of T and
    // not something else.
    pub unsafe fn from_completion(cmpl: &crate::completions::NewQueue) -> Queue<T> {
        Self::new(
            cmpl.id,
            cmpl.size_in_bytes,
            NonNull::new(cmpl.start as *mut ()).expect("queue start pointer is non-null"),
        )
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
    fn move_head(&self) {
        unsafe {
            let head = self.head_ptr.as_ref();
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
    fn move_tail(&self) {
        unsafe {
            let tail = self.tail_ptr.as_ref();
            if tail.fetch_add(1, core::sync::atomic::Ordering::AcqRel) == self.queue_len - 1 {
                tail.store(0, core::sync::atomic::Ordering::Release);
            }
        }
    }

    /// Returns the first outstanding message in the queue if present.
    pub fn poll(&self) -> Option<T> {
        let head = self.head();
        (head != self.tail()).then(|| unsafe {
            let cmd = self.data_ptr.offset(head as isize).read();
            self.move_head();
            cmd
        })
    }

    /// Post a message in the queue.
    pub fn post(&self, msg: &T) -> Result<(), Error> {
        let tail = self.tail();
        (self.head() != (tail + 1) % self.queue_len)
            .then(|| {
                unsafe {
                    let dst = self.data_ptr.offset(tail as isize);
                    core::ptr::copy(msg, dst.as_ptr(), 1);
                }
                self.move_tail();
            })
            .ok_or(Error::Full)
    }
}
