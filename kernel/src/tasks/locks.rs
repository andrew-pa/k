//! Concurrency primitives for tasks.
//!
//! Rather than blocking the entire CPU thread, these allow tasks to yield back to the executor if
//! the resource is not available.

use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicUsize, Ordering},
    task::{Poll, Waker},
};

use alloc::sync::Arc;
use crossbeam::queue::SegQueue;
use futures::Future;

use super::block_on;

/// Inner fields that make up a semaphore but are shared between owners.
struct SemaphoreInner {
    count: AtomicUsize,
    waker_queue: SegQueue<Waker>,
}

/// A semaphore for coordination between tasks.
#[derive(Clone)]
pub struct Semaphore(Arc<SemaphoreInner>);

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
                self.p.waker_queue.push(cx.waker().clone());
                return Poll::Pending;
            } else {
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
        MutexGuard { m: self }
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
}

/// A typical write guard for [RwLock].
pub struct RwLockWriteGuard<'a, T: ?Sized> {
    lock: &'a RwLock<T>,
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
        RwLockReadGuard { lock: self }
    }

    /// Lock for writing (exclusive access). If the data is unavailable, yields until the data becomes available.
    pub async fn write(&self) -> RwLockWriteGuard<T> {
        self.write_mutex.wait().await;
        RwLockWriteGuard { lock: self }
    }
}

impl<'a, T: ?Sized> Drop for RwLockReadGuard<'a, T> {
    fn drop(&mut self) {
        let mut count = block_on(self.lock.reader_count.lock());
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
