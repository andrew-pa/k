use alloc::{collections::VecDeque, vec::Vec};
use spin::{Mutex, MutexGuard};

use super::*;

pub struct ThreadScheduler {
    /// Threads to run at each priority level, forming a queue with (<threads in queue>, <index of queue head>)
    queues: [(Vec<ThreadId>, usize); 3],
    current_thread: ThreadId,
}

impl ThreadScheduler {
    pub fn new(first_thread: ThreadId) -> ThreadScheduler {
        ThreadScheduler {
            queues: [(Vec::new(), 0), (Vec::new(), 0), (Vec::new(), 0)],
            current_thread: first_thread,
        }
    }

    pub fn currently_running(&self) -> ThreadId {
        self.current_thread
    }

    pub fn schedule_next_thread(&mut self) {
        for (queue, next) in self.queues.iter_mut() {
            if queue.len() == 0 {
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
            break;
        }
    }

    pub fn make_task_executor_current(&mut self) {
        self.current_thread = TASK_THREAD;
    }

    pub fn add_thread(&mut self, thread: ThreadId) {
        let t = threads().get(&thread).unwrap();
        self.queues[t.priority as usize].0.push(thread);
    }

    pub fn remove_thread(&mut self, thread: ThreadId) {
        let t = threads().get(&thread).unwrap();
        self.queues[t.priority as usize]
            .0
            .retain(|id| *id != thread);
    }

    pub unsafe fn pause_current_thread(
        &mut self,
        current_regs: *mut Registers,
    ) -> Option<ProcessId> {
        let current = self.currently_running();
        if let Some(mut t) = threads().get_mut(&current) {
            t.save(current_regs.as_ref().unwrap());
            log::trace!("paused thread {current} @ {}, sp={}", t.pc, t.sp);
            t.parent
        } else {
            log::warn!("pausing thread {current} that has no thread info");
            None
        }
    }

    pub unsafe fn resume_thread(
        &mut self,
        id: ThreadId,
        current_regs: *mut Registers,
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

        thread.restore(current_regs.as_mut().unwrap());
    }

    pub unsafe fn resume_current_thread(
        &mut self,
        current_regs: *mut Registers,
        previous_asid: Option<u16>,
    ) {
        self.resume_thread(self.currently_running(), current_regs, previous_asid)
    }
}

// TODO: we will eventually need one of these per-CPU
static mut SCHD: OnceCell<Mutex<ThreadScheduler>> = OnceCell::new();

// TODO: maybe the first thread should always be the idle thread??
pub fn init_scheduler(first_thread: ThreadId) {
    unsafe {
        SCHD.set(Mutex::new(ThreadScheduler::new(first_thread)))
            .ok()
            .expect("init scheduler");
    }
}

// TODO: also will eventually need to be per-CPU?
pub fn scheduler() -> MutexGuard<'static, ThreadScheduler> {
    unsafe { SCHD.get().unwrap().lock() }
}

pub fn current_thread_id() -> Option<ThreadId> {
    unsafe {
        SCHD.get()
            .and_then(|s| s.try_lock())
            .map(|s| s.currently_running())
    }
}
