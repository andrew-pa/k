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

/// A semaphore for coordination between tasks.
#[derive(Clone)]
pub struct Semaphore {
    count: Arc<AtomicUsize>,
    waker_queue: Arc<SegQueue<Waker>>,
}

struct WaitFuture {
    p: Semaphore,
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
    pub fn new(count: usize) -> Semaphore {
        Semaphore {
            count: Arc::new(AtomicUsize::new(count)),
            waker_queue: Default::default(),
        }
    }

    /// Returns a future that waits for the semaphore value to become non-zero.
    /// If the value is non-zero, it will be decremented and the future will resolve.
    pub fn wait(&self) -> impl Future<Output = ()> {
        // Decrements the value of semaphore variable by 1. If the new value of the semaphore variable is negative, the future returned will wait. Otherwise, the task continues execution, having used a unit of the resource.

        WaitFuture { p: self.clone() }
    }

    /// Increments the semaphore value, allowing another waiting task to execute.
    pub fn signal(&self) {
        // Increments the value of semaphore variable by 1. After the increment, if the pre-increment value was negative (meaning there are tasks waiting for a resource), it transfers a blocked task from the semaphore's waiting queue to the ready queue.

        self.count.fetch_add(1, Ordering::AcqRel);
        if let Some(w) = self.waker_queue.pop() {
            w.wake();
        }
    }
}

pub struct Mutex<T: ?Sized> {
    s: Semaphore,
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}

pub struct MutexGuard<'a, T: ?Sized> {
    m: &'a Mutex<T>,
}

impl<T> Mutex<T> {
    pub fn new(data: T) -> Mutex<T> {
        Mutex {
            s: Semaphore::new(1),
            data: UnsafeCell::new(data),
        }
    }
}

impl<T: ?Sized> Mutex<T> {
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

pub struct RwLock<T: ?Sized> {
    reader_count: Mutex<usize>,
    write_mutex: Semaphore,
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send> Send for RwLock<T> {}
unsafe impl<T: ?Sized + Send> Sync for RwLock<T> {}

pub struct RwLockReadGuard<'a, T: ?Sized> {
    lock: &'a RwLock<T>,
}

pub struct RwLockWriteGuard<'a, T: ?Sized> {
    lock: &'a RwLock<T>,
}

impl<T> RwLock<T> {
    pub fn new(data: T) -> Self {
        Self {
            reader_count: Mutex::new(0),
            write_mutex: Semaphore::new(1),
            data: UnsafeCell::new(data),
        }
    }
}

impl<T: ?Sized> RwLock<T> {
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

impl<'a, T: ?Sized> Drop for RwLockWriteGuard<'a, T> {
    fn drop(&mut self) {
        self.lock.write_mutex.signal();
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

impl<'a, T: ?Sized + 'a> DerefMut for RwLockWriteGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.data.get() }
    }
}
