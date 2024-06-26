//! Kernel task executor and async/await infrastructure.
//!
//! The task executor runs in seperate threads and allows the kernel to schedule arbitrary
//! asynchronous tasks.
use alloc::{boxed::Box, rc::Rc, sync::Arc, task::Wake};
use core::{
    cell::OnceCell,
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicU32},
    task::{Context, Poll, Waker},
};
use crossbeam::queue::ArrayQueue;

use hashbrown::HashMap;

pub mod locks;

type TaskId = u32;
type Task = Pin<Box<dyn Future<Output = ()>>>;
type ReadyTaskQueue = Arc<ArrayQueue<TaskId>>;
type NewTaskQueue = Rc<ArrayQueue<(TaskId, Task)>>;

struct TaskWaker {
    id: TaskId,
    ready_queue: ReadyTaskQueue,
}

impl TaskWaker {
    fn new_waker(id: TaskId, ready_queue: ReadyTaskQueue) -> Waker {
        Waker::from(Arc::new(TaskWaker { id, ready_queue }))
    }

    fn wake_task(&self) {
        self.ready_queue.push(self.id).expect("task queue overflow");
    }
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.wake_task()
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.wake_task()
    }
}

/// The executor state.
#[derive(Clone)]
struct Executor {
    ready_queue: ReadyTaskQueue,
    new_task_queue: NewTaskQueue,
    next_task_id: Arc<AtomicU32>,
}

impl Default for Executor {
    fn default() -> Self {
        Self {
            ready_queue: Arc::new(ArrayQueue::new(128)),
            new_task_queue: Rc::new(ArrayQueue::new(32)),
            next_task_id: Arc::new(AtomicU32::new(1)),
        }
    }
}

impl Executor {
    fn spawn(&self, task: impl Future<Output = ()> + 'static) {
        let task_id = self
            .next_task_id
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        log::trace!("spawning task {task_id}");
        match self.new_task_queue.push((task_id, Box::pin(task))) {
            Ok(()) => (),
            Err(_) => panic!("new task queue overflow"),
        }
    }

    /// Run the task executor forever. This is intended to be the effective entry point for the executor thread.
    fn run_forever(self) -> ! {
        let mut tasks = HashMap::new();
        let mut waker_cache = HashMap::new();

        loop {
            // log::trace!("top of loop");
            while let Some((task_id, task)) = self.new_task_queue.pop() {
                // log::trace!("adding new task {task_id}");
                tasks.insert(task_id, task);
                // make sure tasks are only in the ready queue after they have been added to the tasks map
                self.ready_queue
                    .push(task_id)
                    .expect("ready task queue overflow");
            }

            // log::trace!("dequeuing task");
            while let Some(task_id) = self.ready_queue.pop() {
                // log::trace!("polling {task_id}");
                let task = match tasks.get_mut(&task_id) {
                    Some(t) => t,
                    None => continue,
                };
                let waker = waker_cache
                    .entry(task_id)
                    .or_insert_with(|| TaskWaker::new_waker(task_id, self.ready_queue.clone()));
                let mut context = Context::from_waker(waker);
                // log::trace!("polling task {task_id}");
                match task.as_mut().poll(&mut context) {
                    Poll::Ready(()) => {
                        log::trace!("task {task_id} finished");
                        tasks.remove(&task_id);
                        waker_cache.remove(&task_id);
                    }
                    Poll::Pending => {}
                }
            }
            // log::trace!("bottom of loop");
            // super::wait_for_interrupt();
        }
    }
}

// TODO: we will eventually need one of these per-CPU
static mut EXEC: OnceCell<Executor> = OnceCell::new();

/// Initialize the kernel task executor.
pub fn init_executor() {
    unsafe {
        EXEC.set(Executor::default()).ok().expect("init executor");
    }
}

// TODO: also will eventually need to be per-CPU?

/// Spawn an asynchronous task.
// TODO: the task should probably be marked `Send`.
pub fn spawn(task: impl Future<Output = ()> + 'static) {
    let exec = unsafe { EXEC.get().expect("executor initialized") };
    exec.spawn(task);
}

/// The entry point for the task executor thread.
pub fn run_executor() -> ! {
    log::info!("starting task executor");
    let exec = unsafe { EXEC.get().expect("executor initialized").clone() };
    exec.run_forever()
}

struct BlockingWaiter {
    signal: Arc<AtomicBool>,
}

impl Wake for BlockingWaiter {
    fn wake(self: Arc<Self>) {
        self.signal
            .store(false, core::sync::atomic::Ordering::Release);
    }
}

/// Run an asynchronous task on the current thread.
pub fn block_on<O, F: Future<Output = O>>(task: F) -> O {
    let mut task = Box::pin(task);
    let signal = Arc::new(AtomicBool::default());
    let waker = Waker::from(Arc::new(BlockingWaiter {
        signal: signal.clone(),
    }));
    let mut context = Context::from_waker(&waker);
    loop {
        match task.as_mut().poll(&mut context) {
            Poll::Ready(v) => {
                return v;
            }
            Poll::Pending => {
                signal.store(true, core::sync::atomic::Ordering::Release);
                while signal.load(core::sync::atomic::Ordering::Acquire) {
                    core::hint::spin_loop();
                }
            }
        }
    }
}
