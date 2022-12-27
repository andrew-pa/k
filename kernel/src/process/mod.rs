use crate::{
    exception::Registers,
    memory::{paging::PageTable, VirtualAddress},
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

pub struct Thread {
    pub id: ThreadId,
    /// None => kernel thread
    pub parent: Option<ProcessId>,
    pub register_state: Registers,
    pub program_status: SavedProgramStatus,
    pub pc: VirtualAddress,
}

impl Thread {
    /// Save the thread that was interrupted by an exception into this thread
    /// by copying the SPSR and ELR registers
    pub unsafe fn save(&mut self, regs: &Registers) {
        self.register_state = *regs;
        self.program_status = read_saved_program_status();
        self.pc = read_exception_link_reg();
    }

    /// Restore this thread so that it will resume when the kernel finishes processesing an exception
    pub unsafe fn restore(&self, regs: &mut Registers) {
        write_exception_link_reg(self.pc);
        write_saved_program_status(&self.program_status);
        *regs = self.register_state;
    }
}

static mut PROCESSES: OnceCell<CHashMapG<ProcessId, Process>> = OnceCell::new();
static mut THREADS: OnceCell<CHashMapG<ThreadId, Thread>> = OnceCell::new();

pub fn processes() -> &'static CHashMapG<ProcessId, Process> {
    unsafe { PROCESSES.get_or_init(Default::default) }
}

pub fn threads() -> &'static CHashMapG<ThreadId, Thread> {
    unsafe { THREADS.get_or_init(Default::default) }
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
    pub fn default_at_el1() -> SavedProgramStatus {
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

pub fn read_exception_link_reg() -> VirtualAddress {
    let mut v: usize;
    unsafe {
        core::arch::asm!("mrs {v}, ELR_EL1", v = out(reg) v);
    }
    VirtualAddress(v)
}

pub fn write_exception_link_reg(addr: VirtualAddress) {
    unsafe {
        core::arch::asm!("msr ELR_EL1, {v}", v = in(reg) addr.0);
    }
}
