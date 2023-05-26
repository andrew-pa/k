use crate::{
    exception::Registers,
    memory::{paging::PageTable, PhysicalBuffer, VirtualAddress},
    CHashMapG,
};
use bitfield::bitfield;
use core::cell::OnceCell;
use smallvec::SmallVec;

pub mod scheduler;

pub type ProcessId = u32;
pub type ThreadId = u32;

pub struct Process {
    pub id: ProcessId,
    pub page_tables: PageTable,
    pub threads: SmallVec<[ThreadId; 4]>,
}

#[derive(Copy, Clone, Debug)]
pub enum ThreadPriority {
    High = 0,
    Normal = 1,
    Low = 2,
}

pub struct Thread {
    pub id: ThreadId,
    /// None => kernel thread
    pub parent: Option<ProcessId>,
    pub register_state: Registers,
    pub program_status: SavedProgramStatus,
    pub pc: VirtualAddress,
    pub sp: VirtualAddress,
    pub priority: ThreadPriority,
}

impl Thread {
    /// Save the thread that was interrupted by an exception into this thread
    /// by copying the SPSR and ELR registers
    pub unsafe fn save(&mut self, regs: &Registers) {
        self.register_state = *regs;
        self.program_status = read_saved_program_status();
        self.pc = read_exception_link_reg();
        self.sp = read_stack_pointer(0);
    }

    /// Restore this thread so that it will resume when the kernel finishes processesing an exception
    pub unsafe fn restore(&self, regs: &mut Registers) {
        write_exception_link_reg(self.pc);
        write_stack_pointer(0, self.sp);
        write_saved_program_status(&self.program_status);
        *regs = self.register_state;
    }

    pub fn kernel_thread(id: ThreadId, start: fn() -> !, stack: &PhysicalBuffer) -> Self {
        Thread {
            id,
            parent: None,
            register_state: Registers::default(),
            program_status: SavedProgramStatus::initial_for_el1(),
            pc: (start as *const ()).into(),
            sp: stack.virtual_address().offset((stack.len() - 16) as isize),
            priority: ThreadPriority::Normal,
        }
    }
}

pub const IDLE_THREAD: ThreadId = 0;
pub const TASK_THREAD: ThreadId = 1;

static mut PROCESSES: OnceCell<CHashMapG<ProcessId, Process>> = OnceCell::new();
static mut THREADS: OnceCell<CHashMapG<ThreadId, Thread>> = OnceCell::new();

pub fn processes() -> &'static CHashMapG<ProcessId, Process> {
    unsafe { PROCESSES.get_or_init(Default::default) }
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

pub fn spawn_thread(thread: Thread) {
    let id = thread.id;
    threads().insert(id, thread);
    scheduler::scheduler().add_thread(id);
}

bitfield! {
    pub struct SavedProgramStatus(u64);
    impl Debug;
    n, set_n: 31;
    z, set_z: 30;
    c, set_c: 29;
    v, set_v: 28;

    tco, set_tco: 25;
    dit, set_dit: 24;
    uao, set_uao: 23;
    pan, set_pan: 22;
    ss, set_ss: 21;
    il, set_il: 20;

    allint, set_allint: 13;
    ssbs, set_ssbs: 12;
    btype, set_btype: 11, 10;

    d, set_d: 9;
    a, set_a: 8;
    i, set_i: 7;
    f, set_f: 6;

    el, set_el: 3, 2;

    sp, set_sp: 0;
}

impl SavedProgramStatus {
    /// This creates a suitable SPSR value for a thread running at EL0 (using the SP_EL0 stack pointer)
    pub fn initial_for_el0() -> SavedProgramStatus {
        SavedProgramStatus(0)
    }

    /// This creates a suitable SPSR value for a thread running at EL1 with its own stack using the
    /// SP_EL0 stack pointer
    pub fn initial_for_el1() -> SavedProgramStatus {
        let mut spsr = SavedProgramStatus(0);
        spsr.set_el(1);
        spsr
    }
}

pub fn read_saved_program_status() -> SavedProgramStatus {
    let mut v: u64;
    unsafe {
        core::arch::asm!("mrs {v}, SPSR_EL1", v = out(reg) v);
    }
    SavedProgramStatus(v)
}

pub fn write_saved_program_status(spsr: &SavedProgramStatus) {
    unsafe {
        core::arch::asm!("msr SPSR_EL1, {v}", v = in(reg) spsr.0);
    }
}

/// Read the value of the program counter when the exception occured
pub fn read_exception_link_reg() -> VirtualAddress {
    let mut v: usize;
    unsafe {
        core::arch::asm!("mrs {v}, ELR_EL1", v = out(reg) v);
    }
    VirtualAddress(v)
}

/// Write the value that the program counter will assume when the exception handler is finished
pub fn write_exception_link_reg(addr: VirtualAddress) {
    unsafe {
        core::arch::asm!("msr ELR_EL1, {v}", v = in(reg) addr.0);
    }
}

pub fn read_stack_pointer(el: u8) -> VirtualAddress {
    let mut v: usize;
    unsafe {
        match el {
            0 => core::arch::asm!("mrs {v}, SP_EL0", v = out(reg) v),
            1 => core::arch::asm!("mrs {v}, SP_EL1", v = out(reg) v),
            2 => core::arch::asm!("mrs {v}, SP_EL2", v = out(reg) v),
            // 3 => core::arch::asm!("mrs {v}, SP_EL3", v = out(reg) v),
            _ => panic!("invalid exception level {el}"),
        }
    }
    VirtualAddress(v)
}

pub unsafe fn write_stack_pointer(el: u8, sp: VirtualAddress) {
    match el {
        0 => core::arch::asm!("msr SP_EL0, {v}", v = in(reg) sp.0),
        1 => core::arch::asm!("msr SP_EL1, {v}", v = in(reg) sp.0),
        2 => core::arch::asm!("msr SP_EL2, {v}", v = in(reg) sp.0),
        // 3 => core::arch::asm!("msr SP_EL3, {v}", v = in(reg) sp.0),
        _ => panic!("invalid exception level {el}"),
    }
}
