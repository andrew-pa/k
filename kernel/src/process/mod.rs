use crate::{
    exception::Registers,
    memory::{
        paging::{PageTable, PageTableEntryOptions},
        physical_memory_allocator, PhysicalAddress, PhysicalBuffer, VirtualAddress, PAGE_SIZE,
    },
    CHashMapG,
};
use alloc::{boxed::Box, vec::Vec};
use async_trait::async_trait;
use bitfield::{bitfield, Bit};
use core::{cell::OnceCell, error::Error, sync::atomic::AtomicU32};
use futures::Future;
use smallvec::{smallvec, SmallVec};
use snafu::{ensure, OptionExt, ResultExt, Snafu};

pub mod scheduler;

mod spawn;
pub use spawn::{spawn_process, SpawnError};

mod reg;
pub use reg::*;

mod queue;

pub type ProcessId = u32;
pub type ThreadId = u32;

#[async_trait]
pub trait Resource {
    /// Size of the resource in bytes
    fn len(&self) -> usize;

    /// Process an access to an address in the mapped range of the resource that is not yet backed by memory
    async fn on_page_fault(
        &mut self,
        base_address: VirtualAddress,
        accessed_address: VirtualAddress,
        page_tables: &mut PageTable,
    ) -> Result<(), ()>;
}

#[async_trait]
impl<F: crate::fs::File + Send> Resource for F {
    fn len(&self) -> usize {
        self.len() as usize
    }

    async fn on_page_fault(
        &mut self,
        base_address: VirtualAddress,
        accessed_address: VirtualAddress,
        page_tables: &mut PageTable,
    ) -> Result<(), ()> {
        // compute address of affected page
        // allocate new memory
        // map memory in page tables
        // load data from file
        Ok(())
    }
}

pub struct MappedResource {
    base_address: VirtualAddress,
    length: usize,
    resource: Box<dyn Resource>,
}

impl MappedResource {
    fn maps(&self, addr: VirtualAddress) -> bool {
        addr.0
            .checked_sub(self.base_address.0)
            .map(|i| i < self.length)
            .unwrap_or_default()
    }
}

pub struct Process {
    pub id: ProcessId,
    pub page_tables: PageTable,
    pub threads: SmallVec<[ThreadId; 4]>,
    pub mapped_resources: Vec<MappedResource>,
}

#[derive(Copy, Clone, Debug)]
pub enum ThreadPriority {
    High = 0,
    Normal = 1,
    Low = 2,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ThreadState {
    Running,
    Waiting,
}

pub struct Thread {
    pub id: ThreadId,
    /// None => kernel thread
    pub parent: Option<ProcessId>,
    pub state: ThreadState,
    pub register_state: Registers,
    pub program_status: SavedProgramStatus,
    pub pc: VirtualAddress,
    pub sp: VirtualAddress,
    pub priority: ThreadPriority,
}

/// the idle thread is dedicated to handling interrupts, i.e. it is the thread holding the EL1 stack
pub const IDLE_THREAD: ThreadId = 0;
/// the task thread runs the async task executor on its own stack at SP_EL0
pub const TASK_THREAD: ThreadId = 1;

// TODO: what we really want here is a concurrent SlotMap
static mut PROCESSES: OnceCell<CHashMapG<ProcessId, Process>> = OnceCell::new();
static mut THREADS: OnceCell<CHashMapG<ThreadId, Thread>> = OnceCell::new();

static mut NEXT_PID: AtomicU32 = AtomicU32::new(1);
static mut NEXT_TID: AtomicU32 = AtomicU32::new(TASK_THREAD + 1);

pub fn processes() -> &'static CHashMapG<ProcessId, Process> {
    unsafe { PROCESSES.get_or_init(Default::default) }
}

impl Process {
    pub async fn on_page_fault(&mut self, tid: ThreadId, address: VirtualAddress) {
        log::trace!("on_page_fault {address}");
        if let Some(res) = self.mapped_resources.iter_mut().find(|r| r.maps(address)) {
            match res
                .resource
                .on_page_fault(res.base_address, address, &mut self.page_tables)
                .await
            {
                Ok(()) => {
                    // resume thread
                }
                Err(e) => {
                    log::error!(
                        "page fault handler failed for process {} at address {address}",
                        self.id
                    );
                    // TODO: how do we inform the process it has an error?
                }
            }
        } else {
            log::error!(
                "process {}, thread {}: unhandled page fault at {address}",
                self.id,
                tid
            )
        }
    }
}

pub fn threads() -> &'static CHashMapG<ThreadId, Thread> {
    unsafe {
        THREADS.get_or_init(|| {
            let mut ths: CHashMapG<ThreadId, Thread> = Default::default();
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

pub fn next_thread_id() -> ThreadId {
    use core::sync::atomic::Ordering;
    unsafe { NEXT_TID.fetch_add(1, Ordering::AcqRel) }
}

pub fn spawn_thread(thread: Thread) {
    let id = thread.id;
    threads().insert(id, thread);
    scheduler::scheduler().add_thread(id);
}

impl Thread {
    /// Save the current thread state into this thread, assuming an exception is being handled.
    pub unsafe fn save(&mut self, regs: &Registers) {
        self.register_state = *regs;
        self.program_status = read_saved_program_status();
        self.pc = read_exception_link_reg();
        self.sp = read_stack_pointer(0);
    }

    /// Restore this thread so that it will resume when the kernel finishes processesing an exception.
    pub unsafe fn restore(&self, regs: &mut Registers) {
        write_exception_link_reg(self.pc);
        write_stack_pointer(0, self.sp);
        write_saved_program_status(&self.program_status);
        *regs = self.register_state;
    }

    /// Create a new kernel space thread from a entry point function and stack buffer.
    /// For now, caller must ensure stack lives as long as the thread.
    pub fn kernel_thread(id: ThreadId, start: fn() -> !, stack: &PhysicalBuffer) -> Self {
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
            sp: stack.virtual_address().offset((stack.len() - 16) as isize),
            priority: ThreadPriority::Normal,
        }
    }

    /// Create a new user space thread running in EL0.
    pub fn user_thread(
        pid: ProcessId,
        tid: ThreadId,
        pc: VirtualAddress,
        sp: VirtualAddress,
        priority: ThreadPriority,
    ) -> Self {
        Thread {
            id: tid,
            parent: Some(pid),
            state: ThreadState::Running,
            register_state: Registers::default(),
            program_status: SavedProgramStatus::initial_for_el0(),
            pc,
            sp,
            priority,
        }
    }
}
