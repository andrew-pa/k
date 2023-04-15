use alloc::{collections::VecDeque, vec::Vec};
use spin::{Mutex, MutexGuard};

use super::*;

pub struct ThreadScheduler {
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
        for (threads, next_thread) in self.queues.iter_mut() {
            if threads.len() == 0 {
                continue;
            }
            // TODO: check thread state, is it waiting?
            self.current_thread = threads[*next_thread];
            *next_thread = (*next_thread + 1) % threads.len();
            break;
        }
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
}

// TODO: we will eventually need one of these per-CPU
static mut SCHD: OnceCell<Mutex<ThreadScheduler>> = OnceCell::new();

pub unsafe fn init_scheduler(first_thread: ThreadId) {
    SCHD.set(Mutex::new(ThreadScheduler::new(first_thread)))
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
    let previous_asid = {
        let current = scheduler.currently_running();
        log::trace!("pausing thread {current}");
        unsafe {
            if let Some(mut t) = threads().get_mut(&current) {
                t.save(current_regs.as_ref().unwrap());
                log::trace!("pausing thread {current} @ {}", t.pc);
                if let Some(proc) = t.parent.and_then(|id| processes().get(&id)) {
                    Some(proc.page_tables.asid)
                } else {
                    None
                }
            } else {
                None
            }
        }
    };
    scheduler.schedule_next_thread();
    {
        let current = scheduler.currently_running();
        log::trace!("resuming thread {current}");
        let thread = threads()
            .get_mut(&current)
            .expect("scheduler has valid thread IDs");
        log::trace!("resuming thread {current} @ {}", thread.pc);

        unsafe {
            if let Some(proc) = thread.parent.and_then(|id| processes().get(&id)) {
                proc.page_tables.activate();
            }

            crate::memory::paging::flush_tlb_for_asid(previous_asid.unwrap_or(0));

            thread.restore(current_regs.as_mut().unwrap());
        }
    }
}
