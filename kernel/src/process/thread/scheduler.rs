//! Thread scheduling.
use alloc::{collections::VecDeque, vec::Vec};
use spin::{Mutex, MutexGuard};

use crate::exception;

use super::*;

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
    /// Threads to run at each priority level, forming a queue with (<threads in queue>, <index of queue head>)
    queues: [(Vec<ThreadId>, usize); 3],
    current_thread: ThreadId,
}

impl ThreadScheduler {
    /// Create a new scheduler, assuming that the currently running thread is the idle thread.
    ///
    /// The first call to [Self::pause_current_thread] will assign whatever execution state it
    /// finds to the idle thread. The idle thread runs at lowest priority.
    fn new() -> ThreadScheduler {
        ThreadScheduler {
            queues: [
                (Vec::new(), 0),
                (Vec::new(), 0),
                (alloc::vec![IDLE_THREAD], 0),
            ],
            current_thread: IDLE_THREAD,
        }
    }

    /// Get the ID of the thread that is currrently running.
    pub fn currently_running(&self) -> ThreadId {
        self.current_thread
    }

    /// Find the next ready thread and make it current.
    pub fn schedule_next_thread(&mut self) {
        for (queue, next) in self.queues.iter_mut() {
            if queue.is_empty() {
                continue;
            }
            // skip any threads that are waiting in this queue
            let mut skips = queue.len();
            while threads()
                .get(&queue[*next])
                .expect("valid thread ids in queue")
                .state
                == ThreadState::Waiting
            {
                skips -= 1;
                *next = (*next + 1) % queue.len();
            }
            if skips == 0 {
                // every thread in the queue is waiting
                continue;
            }
            self.current_thread = queue[*next];
            *next = (*next + 1) % queue.len();
            return;
        }
        // since there is an idle thread we shouldn't get here.
        panic!("no available thread found to schedule as current");
    }

    /// Force the task executor thread to become current.
    pub fn make_task_executor_current(&mut self) {
        self.current_thread = TASK_THREAD;
    }

    /// Add a thread to the scheduler so that it can run.
    pub fn add_thread(&mut self, thread: ThreadId) {
        let t = threads().get(&thread).unwrap();
        self.queues[t.priority as usize].0.push(thread);
    }

    /// Remove a thread from the scheduler.
    ///
    /// If the thread was current, a new ready thread will be scheduled.
    pub fn remove_thread(&mut self, thread: ThreadId) {
        let t = threads().get(&thread).unwrap();
        self.queues[t.priority as usize]
            .0
            .retain(|id| *id != thread);

        if thread == self.current_thread {
            self.schedule_next_thread();
        }
    }

    /// Pause the currently running thread by saving the current thread execution state into the
    /// thread known to be currently running.
    ///
    /// # Safety
    /// It is assumed that the currently running thread is the thread the scheduler believes is
    /// currently running.
    pub unsafe fn pause_current_thread(
        &mut self,
        current_regs: &mut Registers,
    ) -> (Option<ProcessId>, ThreadId) {
        let current = self.currently_running();
        if let Some(mut t) = threads().get_mut(&current) {
            t.save(current_regs);
            log::trace!("paused thread {current} @ {}, sp={}", t.pc, t.sp);
            (t.parent, current)
        } else {
            log::warn!("pausing thread {current} that has no thread info");
            (None, current)
        }
    }

    unsafe fn resume_thread(
        &mut self,
        id: ThreadId,
        current_regs: &mut Registers,
        previous_asid: Option<u16>,
    ) {
        let thread = threads()
            .get_mut(&id)
            .expect("scheduler has valid thread IDs");
        log::trace!("resuming thread {id} @ {}, sp={}", thread.pc, thread.sp);

        if let Some(proc) = thread.parent.and_then(|id| processes().get(&id)) {
            proc.page_tables.activate();
        }

        crate::memory::paging::flush_tlb_for_asid(previous_asid.unwrap_or(0));

        thread.restore(current_regs);
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
        self.resume_thread(self.currently_running(), current_regs, previous_asid)
    }
}

// TODO: we will eventually need one of these per-CPU
static mut SCHD: OnceCell<Mutex<ThreadScheduler>> = OnceCell::new();

/// Initialize the global thread scheduler. The first thread to run must be the idle thread.
pub fn init_scheduler() {
    unsafe {
        SCHD.set(Mutex::new(ThreadScheduler::new()))
            .ok()
            .expect("init scheduler");
    }
}

// TODO: also will eventually need to be per-CPU?
/// Lock and gain access to the global thread scheduler.
pub fn scheduler() -> MutexGuard<'static, ThreadScheduler> {
    unsafe { SCHD.get().unwrap().lock() }
}

/// Try to read the ID of the current thread running from the global scheduler.
/// If the lock cannot be locked or the scheduler is not yet initialized, then None is returned.
pub fn try_current_thread_id() -> Option<ThreadId> {
    unsafe {
        SCHD.get()
            .and_then(|s| s.try_lock())
            .map(|s| s.currently_running())
    }
}
