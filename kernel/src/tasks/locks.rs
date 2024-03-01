//! Concurrency primitives for tasks.
//!
//! Rather than blocking the entire CPU thread, these allow tasks to yield back to the executor if
//! the resource is not available.

use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::{Poll, Waker},
};

use alloc::sync::Arc;
use crossbeam::queue::SegQueue;
use futures::Future;

use crate::exception::InterruptGuard;

/// A combined waker type for both tasks and threads waiting for the semaphore.
enum SemaphoreWaker {
    /// A typical task waker.
    Task(Waker),
    /// A shared signal with a thread to wake it.
    Thread(NonNull<AtomicBool>),
}

/// # Safety
///
/// The [Waker] is Send, and the thread signal is safe because of atomics and the guarentee that it
/// will remain live until we signal it.
unsafe impl Send for SemaphoreWaker {}

impl SemaphoreWaker {
    /// Signal the waiting task/thread to wake up.
    fn wake(self) {
        match self {
            SemaphoreWaker::Task(t) => t.wake(),
            SemaphoreWaker::Thread(v) => {
                // SAFETY: this pointer is guarenteed to be valid as long as `wait_blocking` only
                // returns after getting signaled.
                unsafe {
                    v.as_ref().store(false, Ordering::Release);
                }
            }
        }
    }
}

/// Inner fields that make up a semaphore but are shared between owners.
struct SemaphoreInner {
    count: AtomicUsize,
    waker_queue: SegQueue<SemaphoreWaker>,
}

/// A semaphore for coordination between tasks.
#[derive(Clone)]
pub struct Semaphore(Arc<SemaphoreInner>);

/// A future that resolves when the semaphore has been acquired.
struct WaitFuture {
    p: Arc<SemaphoreInner>,
}

impl Future for WaitFuture {
    type Output = ();

    fn poll(
        self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> Poll<Self::Output> {
        let mut count = self.p.count.load(Ordering::Acquire);
        loop {
            if count == 0 {
                self.p
                    .waker_queue
                    .push(SemaphoreWaker::Task(cx.waker().clone()));
                return Poll::Pending;
            }

            match self.p.count.compare_exchange(
                count,
                count - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Poll::Ready(()),
                Err(c) => {
                    count = c;
                }
            }
        }
    }
}

impl Semaphore {
    /// Create a new semaphore, initalized with `count`.
    pub fn new(count: usize) -> Semaphore {
        Semaphore(Arc::new(SemaphoreInner {
            count: AtomicUsize::new(count),
            waker_queue: Default::default(),
        }))
    }

    /// Returns a future that waits for the semaphore value to become non-zero.
    /// If the value is non-zero, it will be decremented and the future will resolve.
    pub fn wait(&self) -> impl Future<Output = ()> {
        // Decrements the value of semaphore variable by 1. If the new value of the semaphore variable is negative, the future returned will wait. Otherwise, the task continues execution, having used a unit of the resource.

        WaitFuture { p: self.0.clone() }
    }

    /// Wait for the semaphore value to become non-zero, then decrement it.
    ///
    /// # Safety
    /// If an interrupt handler also interacts with this semaphore, it is possible to create a
    /// deadlock if an interrupt happens after/during a `wait_blocking()` that then itself calls
    /// `wait_blocking()`. To prevent this deadlock, interrupts must be disabled accordingly.
    pub unsafe fn wait_blocking(&self) {
        let mut signal = AtomicBool::new(true);

        let mut count = self.0.count.load(Ordering::Acquire);

        loop {
            // wait for count to become non-zero
            while count == 0 {
                signal.store(true, Ordering::Release);

                self.0
                    .waker_queue
                    .push(SemaphoreWaker::Thread(NonNull::new(&mut signal).unwrap()));

                // wait to get signaled that the count might be non-zero
                while signal.load(core::sync::atomic::Ordering::Acquire) {
                    core::hint::spin_loop();
                }

                count = self.0.count.load(Ordering::Acquire);
            }

            // try to decrement the count
            match self.0.count.compare_exchange(
                count,
                count - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                // if successful we're done
                Ok(_) => return,
                // if the decrement failed, try again from the beginning
                Err(c) => {
                    count = c;
                }
            }
        }
    }

    /// Increments the semaphore value, allowing another waiting task to execute.
    pub fn signal(&self) {
        // Increments the value of semaphore variable by 1. After the increment, if the pre-increment value was negative (meaning there are tasks waiting for a resource), it transfers a blocked task from the semaphore's waiting queue to the ready queue.

        self.0.count.fetch_add(1, Ordering::AcqRel);
        if let Some(w) = self.0.waker_queue.pop() {
            w.wake();
        }
    }
}

/// A mutex for sharing data between tasks.
pub struct Mutex<T: ?Sized> {
    s: Semaphore,
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}

/// A typical mutex guard type for [Mutex].
pub struct MutexGuard<'a, T: ?Sized> {
    m: &'a Mutex<T>,
    ig: Option<InterruptGuard>,
}

impl<T> Mutex<T> {
    /// Create a new Mutex.
    pub fn new(data: T) -> Mutex<T> {
        Mutex {
            s: Semaphore::new(1),
            data: UnsafeCell::new(data),
        }
    }
}

impl<T: ?Sized> Mutex<T> {
    /// Asynchronously lock this mutex. If the mutex is already taken, then this will yield until
    /// it becomes available.
    pub async fn lock(&self) -> MutexGuard<T> {
        self.s.wait().await;
        MutexGuard { m: self, ig: None }
    }

    /// Synchronously lock this mutex. If the mutex is already taken, then this will spin until
    /// it becomes available.
    ///
    /// This function disables interrupts until the guard is dropped to prevent deadlocks.
    pub fn lock_blocking(&self) -> MutexGuard<T> {
        let ig = InterruptGuard::disable_interrupts_until_drop();
        unsafe {
            self.s.wait_blocking();
        }
        MutexGuard {
            m: self,
            ig: Some(ig),
        }
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<'a, T: ?Sized + 'a> Deref for MutexGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.m.data.get() }
    }
}

impl<'a, T: ?Sized + 'a> DerefMut for MutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.m.data.get() }
    }
}

impl<'a, T: ?Sized> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        self.m.s.signal();
    }
}

/// A read/write lock for sharing data between tasks.
pub struct RwLock<T: ?Sized> {
    reader_count: Mutex<usize>,
    write_mutex: Semaphore,
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send> Send for RwLock<T> {}
unsafe impl<T: ?Sized + Send> Sync for RwLock<T> {}

/// A typical read guard for [RwLock].
pub struct RwLockReadGuard<'a, T: ?Sized> {
    lock: &'a RwLock<T>,
    #[allow(unused)] //holding this only for dropping
    ig: Option<InterruptGuard>,
}

/// A typical write guard for [RwLock].
pub struct RwLockWriteGuard<'a, T: ?Sized> {
    lock: &'a RwLock<T>,
    #[allow(unused)] //holding this only for dropping
    ig: Option<InterruptGuard>,
}

impl<T> RwLock<T> {
    /// Create a new RwLock.
    pub fn new(data: T) -> Self {
        Self {
            reader_count: Mutex::new(0),
            write_mutex: Semaphore::new(1),
            data: UnsafeCell::new(data),
        }
    }
}

impl<T: ?Sized> RwLock<T> {
    /// Lock for reading (shared but exclusive with writing). If the data is unavailable, this yields until it becomes available.
    pub async fn read(&self) -> RwLockReadGuard<T> {
        let mut count = self.reader_count.lock().await;
        *count += 1;
        if *count == 1 {
            // this will prevent the reader_count lock from releasing, preventing any additional
            // readers from continuing if a task has the write mutex.
            self.write_mutex.wait().await;
        }
        RwLockReadGuard {
            lock: self,
            ig: None,
        }
    }

    /// Lock for writing (exclusive access). If the data is unavailable, yields until the data becomes available.
    pub async fn write(&self) -> RwLockWriteGuard<T> {
        self.write_mutex.wait().await;
        RwLockWriteGuard {
            lock: self,
            ig: None,
        }
    }

    /// Lock for reading (shared but exclusive with writing). If the data is unavailable, this spins until it becomes available.
    ///
    /// This function is blocking, thus it must be used cautiously if interrupts are expected
    /// within the critical section, or else deadlocks will occur.
    pub fn read_blocking(&self) -> RwLockReadGuard<T> {
        let mut count = self.reader_count.lock_blocking();
        *count += 1;
        if *count == 1 {
            // this will prevent the reader_count lock from releasing, preventing any additional
            // readers from continuing if a task has the write mutex.
            unsafe {
                // SAFETY: this is safe because we just disabled interrupts with `lock_blocking()`.
                self.write_mutex.wait_blocking();
            }
        }
        RwLockReadGuard {
            lock: self,
            // pass the interrupt guard along from the mutex to preserve it
            ig: count.ig.take(),
        }
    }

    /// Lock for writing (exclusive access). If the data is unavailable, this spins until the data becomes available.
    ///
    /// This function is blocking, thus it must be used cautiously if interrupts are expected
    /// within the critical section, or else deadlocks will occur.
    pub fn write_blocking(&self) -> RwLockWriteGuard<T> {
        let ig = InterruptGuard::disable_interrupts_until_drop();
        unsafe {
            // SAFETY: this is safe because we just disabled interrupts with `ig`.
            self.write_mutex.wait_blocking();
        }
        RwLockWriteGuard {
            lock: self,
            ig: Some(ig),
        }
    }
}

impl<'a, T: ?Sized> Drop for RwLockReadGuard<'a, T> {
    fn drop(&mut self) {
        let mut count = self.lock.reader_count.lock_blocking();
        *count -= 1;
        if *count == 0 {
            self.lock.write_mutex.signal();
        }
    }
}

impl<'a, T: ?Sized + 'a> Deref for RwLockReadGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.data.get() }
    }
}

impl<'a, T: ?Sized + 'a> Deref for RwLockWriteGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.data.get() }
    }
}

impl<'a, T: ?Sized> Drop for RwLockWriteGuard<'a, T> {
    fn drop(&mut self) {
        self.lock.write_mutex.signal();
    }
}

impl<'a, T: ?Sized + 'a> DerefMut for RwLockWriteGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.data.get() }
    }
}

/// A mapped read lock guard for [RwLock], allowing a function to return immutable access to only part of a locked data structure.
pub struct MappedRwLockReadGuard<'a, T: ?Sized, U: ?Sized> {
    /// The parent guard.
    #[allow(unused)] //holding this only for dropping
    g: RwLockReadGuard<'a, T>,
    /// A raw reference to the inner data.
    mv: *const U,
}

impl<'a, T: ?Sized + 'a> RwLockReadGuard<'a, T> {
    /// Map the guard to dereference to an inner value in the `T`.
    pub fn map<U>(self, f: impl FnOnce(&T) -> &U) -> MappedRwLockReadGuard<'a, T, U> {
        MappedRwLockReadGuard {
            mv: f(&self),
            g: self,
        }
    }

    /// Try to map the guard to dereference to an inner value in the `T`, returning None if the inner value is None.
    pub fn maybe_map<U>(
        self,
        f: impl FnOnce(&T) -> Option<&U>,
    ) -> Option<MappedRwLockReadGuard<'a, T, U>> {
        #[allow(clippy::manual_map)] // due to the borrow checker
        match f(&self) {
            Some(mv) => Some(MappedRwLockReadGuard { mv, g: self }),
            None => None,
        }
    }
}

impl<'a, T: ?Sized + 'a, U: ?Sized + 'a> Deref for MappedRwLockReadGuard<'a, T, U> {
    type Target = U;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.mv }
    }
}

/// A mapped write lock guard for [RwLock], allowing a function to return mutable access to only part of a locked data structure.
pub struct MappedRwLockWriteGuard<'a, T: ?Sized, U: ?Sized> {
    /// The parent guard.
    #[allow(unused)] //holding this only for dropping
    g: RwLockWriteGuard<'a, T>,
    /// A raw reference to the inner data.
    mv: *mut U,
}

impl<'a, T: ?Sized + 'a> RwLockWriteGuard<'a, T> {
    /// Map the guard to dereference to an inner value in the `T`.
    pub fn map<U>(mut self, f: impl FnOnce(&mut T) -> &mut U) -> MappedRwLockWriteGuard<'a, T, U> {
        MappedRwLockWriteGuard {
            mv: f(&mut self),
            g: self,
        }
    }

    /// Try to map the guard to dereference to an inner value in the `T`, returning None if the inner value is None.
    pub fn maybe_map<U>(
        mut self,
        f: impl FnOnce(&mut T) -> Option<&mut U>,
    ) -> Option<MappedRwLockWriteGuard<'a, T, U>> {
        #[allow(clippy::manual_map)] // due to the borrow checker
        match f(&mut self) {
            Some(mv) => Some(MappedRwLockWriteGuard { mv, g: self }),
            None => None,
        }
    }
}

impl<'a, T: ?Sized + 'a, U: ?Sized + 'a> Deref for MappedRwLockWriteGuard<'a, T, U> {
    type Target = U;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.mv }
    }
}

impl<'a, T: ?Sized + 'a, U: ?Sized + 'a> DerefMut for MappedRwLockWriteGuard<'a, T, U> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.mv }
    }
}
