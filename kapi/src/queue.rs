//! Asynchronous queues for kernel/user-space communication.
//!
//! The implementation is a copy of the `crossbeam-queue` (`ArrayQueue`)[https://github.com/crossbeam-rs/crossbeam/blob/master/crossbeam-queue/src/array_queue.rs] definition, but adapted so that the queue algorithm does not own the memory (so that it can be shared between processes).
use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    num::NonZeroU16,
    ptr::NonNull,
    sync::atomic::{self, AtomicUsize, Ordering},
};

use crossbeam_utils::{Backoff, CachePadded};

/// An ID that uniquely identifies a queue relative to the process it is in.
pub type QueueId = NonZeroU16;
/// The ID of the first receve queue (created by the kernel) for each process.
pub const FIRST_RECV_QUEUE_ID: QueueId = unsafe { NonZeroU16::new_unchecked(1) };
/// The ID of the first send queue (created by the kernel) for each process.
pub const FIRST_SEND_QUEUE_ID: QueueId = unsafe { NonZeroU16::new_unchecked(2) };

/// A slot in the queue.
struct Slot<T> {
    /// The current stamp.
    ///
    /// If the stamp equals the tail, this node will be next written to. If it equals head + 1,
    /// this node will be next read from.
    stamp: AtomicUsize,

    /// The value in this slot.
    value: UnsafeCell<MaybeUninit<T>>,
}

struct Counters {
    head: CachePadded<AtomicUsize>,
    tail: CachePadded<AtomicUsize>,
}

/// A single synchronized queue reference.
///
/// This queue does *not* own the memory that backs it, and does *not* drop the elements when it is
/// dropped!
///
/// A queue is internally mutable and synchronized.
pub struct Queue<T> {
    /// The process-unique ID of this queue.
    pub id: QueueId,
    /// Counters that keep track of the location of the queue in the buffer.
    counter_ptr: NonNull<Counters>,
    /// A pointer into the buffer to the actual data in the queue.
    data_ptr: NonNull<Slot<T>>,
    /// Number of slots in the queue.
    capacity: usize,
    /// A stamp with the value of `{ lap: 1, index: 0 }`.
    one_lap: usize,
}

unsafe impl<T: Send> Send for Queue<T> {}
unsafe impl<T: Send> Sync for Queue<T> {}

/// Compute the total size of a queue in bytes given the capacity and message type.
pub fn queue_size_in_bytes<T>(capacity: usize) -> usize {
    use core::mem::{align_of, size_of};
    let el_size = size_of::<Slot<T>>();
    let el_align = align_of::<Slot<T>>() - 1;
    size_of::<Counters>() + ((el_size + el_align) & !el_align) * capacity
}

impl<T> Queue<T> {
    /// Create a new queue backed by a region of memory.
    ///
    /// # Safety
    /// `backing_memory` must point to a valid region of memory that is at least [queue_size_in_bytes]`(capacity)` bytes long, and that will live at least as long as this Queue instance.
    /// `backing_memory` must also be correctly aligned for atomic load/store operations of `usize`.
    /// The memory region must also have been correctly initialized to be a queue.
    pub unsafe fn new(id: QueueId, backing_memory: NonNull<()>, capacity: usize) -> Queue<T> {
        assert!(capacity > 0);

        let counter_ptr = backing_memory.cast();

        Queue {
            id,
            counter_ptr,
            data_ptr: counter_ptr.add(1).cast(),
            // One lap is the smallest power of two greater than `cap`.
            one_lap: (capacity + 1).next_power_of_two(),
            capacity,
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
    pub unsafe fn from_completion(
        cmpl: &crate::completions::NewQueue,
        capacity: usize,
    ) -> Queue<T> {
        Self::new(
            cmpl.id,
            NonNull::new(cmpl.start as *mut ()).expect("queue start pointer is non-null"),
            capacity,
        )
    }

    /// Initialize the queue. This will remove but not drop anything already in the queue.
    /// This function exists for the use of the kernel, but is unnecessary for user-space
    /// processes.
    ///
    /// # Safety
    /// This is only safe to run once after a queue has been allocated but before it is used.
    /// The queue will not function correctly, however, if it is never called.
    pub unsafe fn initialize(&self) {
        let counters = self.counter_ptr.as_ref();
        counters.head.store(0, Ordering::Relaxed);
        counters.tail.store(0, Ordering::Relaxed);

        for index in 0..self.capacity {
            *self.data_ptr.add(index).as_mut() = Slot {
                stamp: AtomicUsize::new(index),
                value: UnsafeCell::new(MaybeUninit::uninit()),
            };
        }
    }

    /// Number of slots in the queue.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Get an opaque pointer to the start of the queue.
    pub fn root_address(&self) -> NonNull<()> {
        self.counter_ptr.cast()
    }

    fn push_or_else<F>(&self, mut value: T, f: F) -> Result<(), T>
    where
        F: Fn(T, usize, usize, &Slot<T>) -> Result<T, T>,
    {
        let Counters { tail: ctail, .. } = unsafe { self.counter_ptr.as_ref() };

        let backoff = Backoff::new();
        let mut tail = ctail.load(Ordering::Relaxed);

        loop {
            // Deconstruct the tail.
            let index = tail & (self.one_lap - 1);
            let lap = tail & !(self.one_lap - 1);

            let new_tail = if index + 1 < self.capacity {
                // Same lap, incremented index.
                // Set to `{ lap: lap, index: index + 1 }`.
                tail + 1
            } else {
                // One lap forward, index wraps around to zero.
                // Set to `{ lap: lap.wrapping_add(1), index: 0 }`.
                lap.wrapping_add(self.one_lap)
            };

            // Inspect the corresponding slot.
            debug_assert!(index < self.capacity);
            let slot = unsafe { self.data_ptr.add(index).as_ref() };
            let stamp = slot.stamp.load(Ordering::Acquire);

            // If the tail and the stamp match, we may attempt to push.
            if tail == stamp {
                // Try moving the tail.
                match ctail.compare_exchange_weak(
                    tail,
                    new_tail,
                    Ordering::SeqCst,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // Write the value into the slot and update the stamp.
                        unsafe {
                            slot.value.get().write(MaybeUninit::new(value));
                        }
                        slot.stamp.store(tail + 1, Ordering::Release);
                        return Ok(());
                    }
                    Err(t) => {
                        tail = t;
                        backoff.spin();
                    }
                }
            } else if stamp.wrapping_add(self.one_lap) == tail + 1 {
                atomic::fence(Ordering::SeqCst);
                value = f(value, tail, new_tail, slot)?;
                backoff.spin();
                tail = ctail.load(Ordering::Relaxed);
            } else {
                // Snooze because we need to wait for the stamp to get updated.
                backoff.snooze();
                tail = ctail.load(Ordering::Relaxed);
            }
        }
    }

    /// Attempts to push an element into the queue.
    ///
    /// If the queue is full, the element is returned back as an error.
    pub fn post(&self, value: T) -> Result<(), T> {
        self.push_or_else(value, |v, tail, _, _| {
            let Counters { head: chead, .. } = unsafe { self.counter_ptr.as_ref() };
            let head = chead.load(Ordering::Relaxed);

            // If the head lags one lap behind the tail as well...
            if head.wrapping_add(self.one_lap) == tail {
                // ...then the queue is full.
                Err(v)
            } else {
                Ok(v)
            }
        })
    }

    /// Attempts to pop an element from the queue.
    ///
    /// If the queue is empty, `None` is returned.
    pub fn poll(&self) -> Option<T> {
        let Counters {
            head: chead,
            tail: ctail,
        } = unsafe { self.counter_ptr.as_ref() };
        let backoff = Backoff::new();
        let mut head = chead.load(Ordering::Relaxed);

        loop {
            // Deconstruct the head.
            let index = head & (self.one_lap - 1);
            let lap = head & !(self.one_lap - 1);

            // Inspect the corresponding slot.
            debug_assert!(index < self.capacity);
            let slot = unsafe { self.data_ptr.add(index).as_ref() };
            let stamp = slot.stamp.load(Ordering::Acquire);

            // If the stamp is ahead of the head by 1, we may attempt to pop.
            if head + 1 == stamp {
                let new = if index + 1 < self.capacity {
                    // Same lap, incremented index.
                    // Set to `{ lap: lap, index: index + 1 }`.
                    head + 1
                } else {
                    // One lap forward, index wraps around to zero.
                    // Set to `{ lap: lap.wrapping_add(1), index: 0 }`.
                    lap.wrapping_add(self.one_lap)
                };

                // Try moving the head.
                match chead.compare_exchange_weak(head, new, Ordering::SeqCst, Ordering::Relaxed) {
                    Ok(_) => {
                        // Read the value from the slot and update the stamp.
                        let msg = unsafe { slot.value.get().read().assume_init() };
                        slot.stamp
                            .store(head.wrapping_add(self.one_lap), Ordering::Release);
                        return Some(msg);
                    }
                    Err(h) => {
                        head = h;
                        backoff.spin();
                    }
                }
            } else if stamp == head {
                atomic::fence(Ordering::SeqCst);
                let tail = ctail.load(Ordering::Relaxed);

                // If the tail equals the head, that means the channel is empty.
                if tail == head {
                    return None;
                }

                backoff.spin();
                head = chead.load(Ordering::Relaxed);
            } else {
                // Snooze because we need to wait for the stamp to get updated.
                backoff.snooze();
                head = chead.load(Ordering::Relaxed);
            }
        }
    }
}
