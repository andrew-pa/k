//! Thread scheduling.
use alloc::{sync::Arc, vec::Vec};
use spin::{once::Once, Mutex, MutexGuard};

use crate::process::ThreadState;

use super::{thread_for_id, Registers, Thread, ThreadId, IDLE_THREAD, TASK_THREAD};

/// The thread scheduler.
///
/// This is a simple round-robin scheduler with a simple priority strategy.
/// Highest priority threads are considered first in order. If none are ready to run, then the next lowest
/// priority is considered the same way, and so on.
///
/// There must be at least one thread that is always ready or the scheduler will panic.
/// This should be the [IDLE_THREAD].
// TODO: make sure that all threads get a chance to run at least occasionally.
pub struct ThreadScheduler {
    /// Threads to run at each priority level, forming a queue with (<threads in queue>, <index of queue head>).
    /// This is only synchronized to appease the compiler, the scheduler should only ever run on its own core.
    queues: [(Vec<Arc<Thread>>, usize); 3],
    /// Reference to current thread.
    current_thread: Arc<Thread>,
}

impl ThreadScheduler {
    /// Create a new scheduler, assuming that the currently running thread is the idle thread.
    ///
    /// The first call to [Self::pause_current_thread] will assign whatever execution state it
    /// finds to the idle thread. The idle thread runs at lowest priority.
    fn new() -> ThreadScheduler {
        let idle = thread_for_id(IDLE_THREAD).expect("idle thread exists");
        ThreadScheduler {
            queues: [
                (Vec::new(), 0),
                (Vec::new(), 0),
                (alloc::vec![idle.clone()], 0),
            ],
            current_thread: idle,
        }
    }

    /// Find the next ready thread and make it current.
    pub fn schedule_next_thread(&mut self) {
        for (queue, next) in self.queues.iter_mut() {
            if queue.is_empty() {
                continue;
            }
            // skip any threads that are waiting in this queue
            let mut skips = queue.len();
            while queue[*next].state() != ThreadState::Running {
                skips -= 1;
                *next = (*next + 1) % queue.len();
            }
            if skips == 0 {
                // every thread in the queue is waiting
                continue;
            }
            self.current_thread = queue[*next].clone();
            *next = (*next + 1) % queue.len();
            return;
        }
        // since there is an idle thread we shouldn't get here.
        panic!("no available thread found to schedule as current");
    }

    /// Force the task executor thread to become current.
    pub fn make_task_executor_current(&mut self) {
        self.current_thread = thread_for_id(TASK_THREAD).expect("task thread exists");
    }

    /// Add a thread to the scheduler so that it can run.
    pub fn add_thread(&mut self, thread: Arc<Thread>) {
        self.queues[thread.priority() as usize].0.push(thread);
    }

    /// Remove a thread from the scheduler.
    ///
    /// If the thread was current, a new ready thread will be scheduled.
    pub fn remove_thread(&mut self, thread: &Arc<Thread>) {
        self.queues[thread.priority() as usize]
            .0
            .retain(|t| t.id != thread.id);

        if thread.id == self.current_thread.id {
            self.schedule_next_thread();
        }
    }

    /// Pause the currently running thread by saving the current thread execution state into the
    /// thread known to be currently running.
    ///
    /// # Safety
    /// It is assumed that the currently running thread is the thread the scheduler believes is
    /// currently running.
    pub unsafe fn pause_current_thread(&mut self, current_regs: &mut Registers) -> Arc<Thread> {
        {
            let mut exec_state = self.current_thread.exec_state.lock();
            exec_state.save(current_regs);
            log::trace!(
                "paused thread {} @ {}, sp={}",
                self.current_thread.id,
                exec_state.pc,
                exec_state.sp
            );
        }
        self.current_thread.clone()
    }

    /// Resume the execution state of the thread that the scheduler considers current.
    ///
    /// # Safety
    /// It is assumed that the context of this call is such that resuming the thread will be
    /// correct. In other words, when returning from an exception.
    pub unsafe fn resume_current_thread(
        &mut self,
        current_regs: &mut Registers,
        previous_asid: Option<u16>,
    ) {
        let exec_state = self.current_thread.exec_state.lock();
        log::trace!(
            "resuming thread {} @ {}, sp={}",
            self.current_thread.id,
            exec_state.pc,
            exec_state.sp
        );

        if let Some(proc) = self.current_thread.parent.as_ref() {
            proc.page_tables.activate();
        }

        crate::memory::paging::flush_tlb_for_asid(previous_asid.unwrap_or(0));

        exec_state.restore(current_regs);
    }
}

static SCHD: Once<Mutex<ThreadScheduler>> = Once::new();

/// Initialize the global thread scheduler. The first thread to run must be the idle thread.
pub fn init_scheduler() {
    SCHD.call_once(|| Mutex::new(ThreadScheduler::new()));
}

// TODO: also will eventually need to be per-CPU?
/// Lock and gain access to the global thread scheduler.
pub fn scheduler() -> MutexGuard<'static, ThreadScheduler> {
    SCHD.get().expect("thread scheduler initialized").lock()
}

/// Try to read the ID of the current thread running from the global scheduler.
/// If the lock cannot be locked or the scheduler is not yet initialized, then None is returned.
pub fn try_current_thread_id() -> Option<ThreadId> {
    SCHD.get()
        .and_then(|s| s.try_lock())
        .map(|s| s.current_thread.id)
}
