//! Threads represent a single path of execution within a process.
use core::sync::atomic::{AtomicU32, AtomicU8, Ordering};

use alloc::sync::Arc;
use futures::Future;
use kapi::completions::ThreadExit;
use spin::{once::Once, Mutex};

use bytemuck::Contiguous;

use crate::{
    ds::lists::ConcurrentLinkedList,
    exception::Registers,
    memory::{
        paging::kernel_table, physical_memory_allocator, virtual_address_allocator, PhysicalBuffer,
        VirtualAddress,
    },
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
    exit_state: Arc<super::ExitState>,
    /// The base address of the stack for this thread.
    /// This address is relative to the parent's address space.
    stack_start: VirtualAddress,
    /// The length in pages of this thread's stack.
    stack_page_count: usize,
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
            // TODO: we don't know these values, but we probably should?!
            stack_start: VirtualAddress(0),
            stack_page_count: 0,
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
    pub fn kernel_thread(id: ThreadId, start: fn() -> !, stack: PhysicalBuffer) -> Self {
        let t = Thread {
            id,
            parent: None,
            state: ThreadState::Running.into_integer().into(),
            priority: ThreadPriority::Normal.into_integer().into(),
            exec_state: Mutex::new(ExecutionState {
                register_state: Registers::default(),
                program_status: SavedProgramStatus::initial_for_el1(),
                pc: (start as *const ()).into(),
                sp: stack.virtual_address().add(stack.len()),
            }),
            exit_state: Default::default(),
            stack_start: stack.virtual_address(),
            stack_page_count: stack.page_count(),
        };
        // the thread takes ownership of the physical and virtual memory of the stack
        core::mem::forget(stack);
        t
    }

    /// Create a new user space thread running in EL0.
    pub fn user_thread(
        proc: Arc<Process>,
        tid: ThreadId,
        entry_point: VirtualAddress,
        stack_base_address: VirtualAddress,
        stack_page_count: usize,
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
                sp: stack_base_address.add(stack_page_count * crate::memory::PAGE_SIZE),
            }),
            exit_state: Default::default(),
            stack_start: stack_base_address,
            stack_page_count,
        }
    }

    /// Create a future that will resolve with the value of the exit code when this thread exits.
    pub fn exit_code(&self) -> impl Future<Output = ThreadExit> {
        super::ExitFuture {
            state: self.exit_state.clone(),
        }
    }

    /// Cause a thread to exit, freeing its resources. If this is the last thread in a process, then
    /// the process will also exit.
    pub fn exit(self: &Arc<Thread>, exit_code: ThreadExit) {
        log::trace!("thread {} exited with code {exit_code:?}", self.id);

        scheduler::scheduler().remove_thread(self);
        threads().remove(|t| t.id == self.id);

        let proc_if_last_thread = if let Some(p) = self.parent.as_ref() {
            let mut ts = p.threads.lock_blocking();
            ts.retain(|t| t.id != self.id);

            // free the stack, while we're here
            p.free_memory(self.stack_start, self.stack_page_count);

            ts.is_empty().then_some(p)
        } else {
            // this is a kernel thread (no parent), so we need to free the kernel memory that holds our stack
            kernel_table().unmap_range_custom(self.stack_start, self.stack_page_count, |region| {
                physical_memory_allocator().free_pages(region.base_phys_addr, region.page_count);
            });
            virtual_address_allocator().free(self.stack_start, self.stack_page_count);

            None
        };

        self.exit_state.set(exit_code.clone());

        if let Some(proc) = proc_if_last_thread {
            // this thread was the last, so the process itself is now dead.
            log::trace!("process {} exited", proc.id);
            proc.exit_state.set(exit_code);
            super::processes().remove(|p| p.id == proc.id);
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
