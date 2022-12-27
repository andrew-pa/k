use alloc::collections::VecDeque;
use spin::{Mutex, MutexGuard};

use super::*;

pub struct ThreadScheduler {
    queue: VecDeque<ThreadId>,
}

impl ThreadScheduler {
    pub fn new() -> ThreadScheduler {
        ThreadScheduler {
            queue: VecDeque::new(),
        }
    }

    pub fn currently_running(&self) -> Option<ThreadId> {
        self.queue.front().copied()
    }

    pub fn schedule_next_thread(&mut self) {
        // TODO: a more sophisticated method to choose the next thread
        self.queue.rotate_right(1);
    }

    pub fn add_thread(&mut self, thread: ThreadId) {
        self.queue.push_back(thread);
    }

    pub fn remove_thread(&mut self, thread: ThreadId) {
        if let Some((i, _)) = self.queue.iter().enumerate().find(|(_, id)| **id == thread) {
            self.queue.swap_remove_back(i);
        }
    }
}

// TODO: we will eventually need one of these per-CPU
static mut SCHD: OnceCell<Mutex<ThreadScheduler>> = OnceCell::new();

pub unsafe fn init_scheduler() {
    SCHD.set(Mutex::new(ThreadScheduler::new()))
        .ok()
        .expect("init scheduler");
}

// TODO: also will eventually need to be per-CPU?
pub fn scheduler() -> MutexGuard<'static, ThreadScheduler> {
    unsafe { SCHD.get().unwrap().lock() }
}

pub fn run_scheduler(current_regs: *mut Registers) {
    let mut scheduler = unsafe {
        if let Some(s) = SCHD.get_mut().unwrap().try_lock() {
            s
        } else {
            // try again next cycle
            return;
        }
    };
    if let Some(current) = scheduler.currently_running() {
        log::trace!("pausing thread {current}");
        unsafe {
            threads()
                .get_mut(&current)
                .expect("scheduler has valid thread IDs")
                .save(current_regs.as_ref().unwrap());
        }
    }
    scheduler.schedule_next_thread();
    if let Some(current) = scheduler.currently_running() {
        log::trace!("resuming thread {current}");
        let thread = threads()
            .get_mut(&current)
            .expect("scheduler has valid thread IDs");

        unsafe {
            if let Some(proc) = thread.parent.and_then(|id| processes().get(&id)) {
                // TODO: set ASID
                proc.page_tables.activate();
            }

            thread.restore(current_regs.as_mut().unwrap());
        }
    }
}
