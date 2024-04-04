use core::sync::atomic::{AtomicU32, AtomicU8, Ordering};

use alloc::sync::Arc;
use futures::Future;
use kapi::completions::ThreadExit;
use smallvec::SmallVec;
use spin::{once::Once, Mutex};

use bytemuck::Contiguous;

use crate::{
    ds::lists::ConcurrentLinkedList,
    exception::Registers,
    memory::{PhysicalBuffer, VirtualAddress},
};

use super::Process;

pub mod reg;
pub mod scheduler;
use reg::*;

pub use kapi::ThreadId;

/// The priority of a thread in the scheduler.
#[repr(u8)]
#[derive(Copy, Clone, Debug)]
pub enum ThreadPriority {
    High = 0,
    Normal = 1,
    Low = 2,
}

unsafe impl Contiguous for ThreadPriority {
    type Int = u8;

    const MIN_VALUE: u8 = 0;
    const MAX_VALUE: u8 = 2;
}

/// The state of a thread for scheduling purposes.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ThreadState {
    /// The thread can become current/is currently executing.
    Running = 1,
    /// The thread is waiting for something to finish and cannot become current.
    Waiting = 2,
}

unsafe impl Contiguous for ThreadState {
    type Int = u8;

    const MIN_VALUE: Self::Int = 1;
    const MAX_VALUE: Self::Int = 2;
}

struct ExecutionState {
    register_state: Registers,
    program_status: SavedProgramStatus,
    pc: VirtualAddress,
    sp: VirtualAddress,
}

#[derive(Default)]
struct ExitState {
    /// The exit code this process exited with (if it has exited).
    code: Once<ThreadExit>,
    /// Future wakers for futures waiting on exit code.
    wakers: Mutex<SmallVec<[core::task::Waker; 1]>>,
}

/// A thread is a single unit of user-space code execution, happening in the context of some
/// process.
pub struct Thread {
    pub id: ThreadId,
    /// None => kernel thread
    pub parent: Option<Arc<Process>>,
    /// An atomic [ThreadState].
    state: AtomicU8,
    /// An atomic [ThreadPriority].
    priority: AtomicU8,
    exec_state: Mutex<ExecutionState>,
    exit_state: Arc<ExitState>,
}

crate::assert_sync!(Thread);

/// The idle thread is dedicated to handling interrupts, i.e. it is the thread holding the EL1 stack.
pub const IDLE_THREAD: ThreadId = 0;
/// The task thread runs the async task executor on its own stack at SP_EL0.
pub const TASK_THREAD: ThreadId = 1;

static THREADS: Once<ConcurrentLinkedList<Arc<Thread>>> = Once::new();

/// The global tables of threads by ID.
pub fn threads() -> &'static ConcurrentLinkedList<Arc<Thread>> {
    THREADS.call_once(|| {
        let ths: ConcurrentLinkedList<Arc<Thread>> = Default::default();
        // Create the idle thread, which will just wait for interrupts
        let mut program_status = SavedProgramStatus::initial_for_el1();
        program_status.set_sp(true); // the idle thread runs on the EL1 stack normally used by interrupts and kmain
        ths.push(Arc::new(Thread {
            id: IDLE_THREAD,
            parent: None,
            state: ThreadState::Running.into_integer().into(),
            priority: ThreadPriority::Low.into_integer().into(),
            exec_state: Mutex::new(ExecutionState {
                register_state: Registers::default(),
                program_status,
                pc: VirtualAddress(0),
                sp: VirtualAddress(0),
            }),
            exit_state: Default::default(),
        }));
        ths
    })
}

/// Retrieve a thread by its id.
pub fn thread_for_id(tid: ThreadId) -> Option<Arc<Thread>> {
    threads().iter().find(|t| t.id == tid).cloned()
}

static NEXT_TID: AtomicU32 = AtomicU32::new(TASK_THREAD + 1);

/// Get the next free thread ID.
pub fn next_thread_id() -> ThreadId {
    NEXT_TID.fetch_add(1, Ordering::AcqRel)
}

/// Spawn a thread. It is up to the caller to ensure that this thread is valid.
///
/// This inserts the thread in the global table and also adds it to the scheduler.
pub fn spawn_thread(thread: Arc<Thread>) {
    // TODO: async version?
    // TODO: check to make sure the thread ID is actually unique
    threads().push(thread.clone());
    scheduler::scheduler().add_thread(thread);
}

impl ExecutionState {
    /// Save the current thread state into this thread, assuming an exception is being handled.
    pub fn save(&mut self, regs: &Registers) {
        self.register_state = *regs;
        self.program_status = read_saved_program_status();
        self.pc = read_exception_link_reg();
        self.sp = read_stack_pointer(0);
    }

    /// Restore this thread so that it will resume when the kernel finishes processesing an exception.
    ///
    /// # Safety
    /// This function writes to the program counter/program status register.
    /// The caller is responsible for making sure that this `Thread` contains valid state before
    /// attempting to restore that state, otherwise the processor could start to execute in an
    /// unexpected/invalid state.
    pub unsafe fn restore(&self, regs: &mut Registers) {
        write_exception_link_reg(self.pc);
        write_stack_pointer(0, self.sp);
        write_saved_program_status(&self.program_status);
        *regs = self.register_state;
    }
}

impl Thread {
    /// Create a new kernel space thread from a entry point function and stack buffer.
    ///
    /// # Safety
    /// For now, the caller must ensure the stack memory lives as long as the thread.
    pub unsafe fn kernel_thread(id: ThreadId, start: fn() -> !, stack: &PhysicalBuffer) -> Self {
        // TODO: the stack needs to stick around for the entire runtime of the thread, but right
        // now since we only have a borrow the caller could then immediately drop the stack buffer
        // and cause the thread to use unallocated memory as stack.
        Thread {
            id,
            parent: None,
            state: ThreadState::Running.into_integer().into(),
            priority: ThreadPriority::Normal.into_integer().into(),
            exec_state: Mutex::new(ExecutionState {
                register_state: Registers::default(),
                program_status: SavedProgramStatus::initial_for_el1(),
                pc: (start as *const ()).into(),
                sp: stack.virtual_address().add(stack.len() - 16),
            }),
            exit_state: Default::default(),
        }
    }

    /// Create a new user space thread running in EL0.
    pub fn user_thread(
        proc: Arc<Process>,
        tid: ThreadId,
        entry_point: VirtualAddress,
        initial_stack_pointer: VirtualAddress,
        priority: ThreadPriority,
        start_registers: Registers,
    ) -> Self {
        Thread {
            id: tid,
            parent: Some(proc),
            state: ThreadState::Running.into_integer().into(),
            priority: priority.into_integer().into(),
            exec_state: Mutex::new(ExecutionState {
                register_state: start_registers,
                program_status: SavedProgramStatus::initial_for_el0(),
                pc: entry_point,
                sp: initial_stack_pointer,
            }),
            exit_state: Default::default(),
        }
    }

    /// Create a future that will resolve with the value of the exit code when this thread exits.
    pub fn exit_code(&self) -> impl Future<Output = ThreadExit> {
        struct ExitFuture {
            state: Arc<ExitState>,
        }

        impl Future for ExitFuture {
            type Output = ThreadExit;

            fn poll(
                self: core::pin::Pin<&mut Self>,
                cx: &mut core::task::Context<'_>,
            ) -> core::task::Poll<Self::Output> {
                match self.state.code.poll() {
                    Some(c) => core::task::Poll::Ready(c.clone()),
                    None => {
                        self.state.wakers.lock().push(cx.waker().clone());
                        core::task::Poll::Pending
                    }
                }
            }
        }

        ExitFuture {
            state: self.exit_state.clone(),
        }
    }

    /// Cause a thread to exit, freeing its resources. If this is the last thread in a process, then
    /// the process will also exit.
    pub fn exit(self: &Arc<Thread>, exit_code: ThreadExit) {
        log::trace!("thread {} exited with code {exit_code:?}", self.id);

        scheduler::scheduler().remove_thread(self);
        threads().remove(|t| t.id == self.id);

        let pid_if_last_thread = if let Some(p) = self.parent.as_ref() {
            let mut ts = p.threads.lock_blocking();
            ts.retain(|t| t.id != self.id);
            ts.is_empty().then_some(p.id)
        } else {
            None
        };

        self.exit_state.code.call_once(|| exit_code);
        for w in self.exit_state.wakers.lock().drain(..) {
            w.wake();
        }

        if let Some(pid) = pid_if_last_thread {
            // this thread was the last, so the process itself is now dead.
            log::trace!("process {pid} exited");
            super::processes().remove(|p| p.id == pid);
        }
    }

    /// Get the state of this thread, atomically.
    pub fn state(&self) -> ThreadState {
        ThreadState::from_integer(self.state.load(Ordering::Acquire))
            .expect("thread state is valid")
    }

    /// Atomically set the state of this thread.
    pub fn set_state(&self, new: ThreadState) {
        self.state.store(new.into_integer(), Ordering::Release)
    }

    /// Get the priority of this thread, atomically.
    pub fn priority(&self) -> ThreadPriority {
        ThreadPriority::from_integer(self.priority.load(Ordering::Acquire))
            .expect("thread state is valid")
    }
}
