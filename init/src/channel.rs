// TODO: redudant with kernel/src/process/queue.rs?
// TODO: move to kapi?

use core::{ptr::NonNull, sync::atomic::AtomicUsize};

use kapi::{Command, Completion};

struct Queue<T> {
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
    unsafe fn new(base_addr: usize, size_in_bytes: usize) -> Queue<T> {
        // compute the number of slots in the queue
        let queue_len =
            (size_in_bytes - (core::mem::size_of::<AtomicUsize>() * 2)) / core::mem::size_of::<T>();

        // place the head/tail pointers at the start of the buffer
        let p: *mut AtomicUsize = base_addr as *mut AtomicUsize;

        // put the messages after the head and tail pointers in the buffer
        let data_ptr: *mut T = (base_addr + (core::mem::size_of::<AtomicUsize>() * 2)) as *mut T;

        Queue {
            queue_len,
            head_ptr: NonNull::new(p).unwrap(),
            tail_ptr: NonNull::new(unsafe { p.offset(1) }).unwrap(),
            data_ptr: NonNull::new(data_ptr).unwrap(),
        }
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
}

pub struct Channel {
    submission: Queue<Command>,
    completion: Queue<Completion>,
}

impl Channel {
    pub unsafe fn from_kernel(
        channel_base_addr: usize,
        channel_sub_size: usize,
        channel_com_size: usize,
    ) -> Channel {
        Channel {
            submission: Queue::new(channel_base_addr, channel_sub_size),
            completion: Queue::new(channel_base_addr + channel_sub_size, channel_com_size),
        }
    }

    /// Returns the first outstanding completion message in the channel if present.
    pub fn poll(&mut self) -> Option<Completion> {
        let tail = self.completion.tail();
        if self.completion.head() != tail {
            unsafe {
                let cmd = self.completion.data_ptr.offset(tail as isize).read();
                self.completion.move_tail();
                Some(cmd)
            }
        } else {
            None
        }
    }

    /// Post a command message in the channel.
    pub fn post(&mut self, msg: &Command) -> Result<(), ()> {
        let head = self.submission.head();
        if head == self.submission.tail() {
            Err(())
        } else {
            unsafe {
                let dst = self.submission.data_ptr.offset(head as isize);
                core::ptr::copy(msg, dst.as_ptr(), 1);
            }
            self.submission.move_head();
            Ok(())
        }
    }
}
