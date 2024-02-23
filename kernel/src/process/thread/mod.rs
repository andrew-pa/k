pub mod reg;
use reg::*;
pub mod scheduler;

use super::*;

/// The system-wide unique ID of a thread.
pub type ThreadId = u32;

/// The priority of a thread in the scheduler.
#[derive(Copy, Clone, Debug)]
pub enum ThreadPriority {
    High = 0,
    Normal = 1,
    Low = 2,
}

/// The state of a thread for scheduling purposes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ThreadState {
    /// The thread can become current/is currently executing.
    Running,
    /// The thread is waiting for something to finish and cannot become current.
    Waiting,
}

/// A thread is a single unit of user-space code execution, happening in the context of some
/// process.
pub struct Thread {
    pub id: ThreadId,
    /// None => kernel thread
    pub parent: Option<ProcessId>,
    pub state: ThreadState,
    pub priority: ThreadPriority,
    register_state: Registers,
    program_status: SavedProgramStatus,
    pc: VirtualAddress,
    sp: VirtualAddress,
}

/// The idle thread is dedicated to handling interrupts, i.e. it is the thread holding the EL1 stack.
pub const IDLE_THREAD: ThreadId = 0;
/// The task thread runs the async task executor on its own stack at SP_EL0.
pub const TASK_THREAD: ThreadId = 1;

static mut THREADS: OnceCell<CHashMap<ThreadId, Thread>> = OnceCell::new();

/// The global tables of threads by ID.
pub fn threads() -> &'static CHashMap<ThreadId, Thread> {
    unsafe {
        THREADS.get_or_init(|| {
            let mut ths: CHashMap<ThreadId, Thread> = Default::default();
            // Create the idle thread, which will just wait for interrupts
            let mut program_status = SavedProgramStatus::initial_for_el1();
            program_status.set_sp(true); // the idle thread runs on the EL1 stack normally used by interrupts and kmain
            ths.insert(
                IDLE_THREAD,
                Thread {
                    id: IDLE_THREAD,
                    parent: None,
                    state: ThreadState::Running,
                    register_state: Registers::default(),
                    program_status,
                    pc: VirtualAddress(0),
                    sp: VirtualAddress(0),
                    priority: ThreadPriority::Low,
                },
            );
            ths
        })
    }
}

static mut NEXT_TID: AtomicU32 = AtomicU32::new(TASK_THREAD + 1);

/// Get the next free thread ID.
pub fn next_thread_id() -> ThreadId {
    use core::sync::atomic::Ordering;
    unsafe { NEXT_TID.fetch_add(1, Ordering::AcqRel) }
}

/// Spawn a thread. It is up to the caller to ensure that this thread is valid.
///
/// This inserts the thread in the global table and also adds it to the scheduler.
pub fn spawn_thread(thread: Thread) {
    let id = thread.id;
    threads().insert(id, thread);
    scheduler::scheduler().add_thread(id);
}

impl Thread {
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
            state: ThreadState::Running,
            register_state: Registers::default(),
            program_status: SavedProgramStatus::initial_for_el1(),
            pc: (start as *const ()).into(),
            sp: stack.virtual_address().add(stack.len() - 16),
            priority: ThreadPriority::Normal,
        }
    }

    /// Create a new user space thread running in EL0.
    pub fn user_thread(
        pid: ProcessId,
        tid: ThreadId,
        entry_point: VirtualAddress,
        initial_stack_pointer: VirtualAddress,
        priority: ThreadPriority,
        start_registers: Registers,
    ) -> Self {
        Thread {
            id: tid,
            parent: Some(pid),
            state: ThreadState::Running,
            register_state: start_registers,
            program_status: SavedProgramStatus::initial_for_el0(),
            pc: entry_point,
            sp: initial_stack_pointer,
            priority,
        }
    }
}
