//! Registers related to threads/execution state.
use bitfield::bitfield;

use crate::memory::VirtualAddress;

bitfield! {
    /// The value of the SPSR (Saved Program Status) register.
    pub struct SavedProgramStatus(u64);
    impl Debug;
    pub n, set_n: 31;
    pub z, set_z: 30;
    pub c, set_c: 29;
    pub v, set_v: 28;

    pub tco, set_tco: 25;
    pub dit, set_dit: 24;
    pub uao, set_uao: 23;
    pub pan, set_pan: 22;
    pub ss, set_ss: 21;
    pub il, set_il: 20;

    pub allint, set_allint: 13;
    pub ssbs, set_ssbs: 12;
    pub btype, set_btype: 11, 10;

    pub d, set_d: 9;
    pub a, set_a: 8;
    pub i, set_i: 7;
    pub f, set_f: 6;

    pub el, set_el: 3, 2;

    pub sp, set_sp: 0;
}

impl SavedProgramStatus {
    /// Creates a suitable SPSR value for a thread running at EL0 (using the SP_EL0 stack pointer).
    pub fn initial_for_el0() -> SavedProgramStatus {
        SavedProgramStatus(0)
    }

    /// Creates a suitable SPSR value for a thread running at EL1 with its own stack using the
    /// SP_EL0 stack pointer.
    pub fn initial_for_el1() -> SavedProgramStatus {
        let mut spsr = SavedProgramStatus(0);
        spsr.set_el(1);
        spsr
    }
}

/// Read the current value of the SPSR_EL1 register.
pub fn read_saved_program_status() -> SavedProgramStatus {
    let mut v: u64;
    unsafe {
        core::arch::asm!("mrs {v}, SPSR_EL1", v = out(reg) v);
    }
    SavedProgramStatus(v)
}

/// Write to the SPSR_EL1 register.
///
/// # Safety
/// It is up to the caller to ensure that the SavedProgramStatus value is correct.
pub unsafe fn write_saved_program_status(spsr: &SavedProgramStatus) {
    core::arch::asm!("msr SPSR_EL1, {v}", v = in(reg) spsr.0);
}

/// Read the value of the program counter when the exception occured.
pub fn read_exception_link_reg() -> VirtualAddress {
    let mut v: usize;
    unsafe {
        core::arch::asm!("mrs {v}, ELR_EL1", v = out(reg) v);
    }
    VirtualAddress(v)
}

/// Write the value that the program counter will assume when the exception handler is finished.
///
/// # Safety
/// It is up to the caller to ensure that the address is valid to store as the program counter.
pub unsafe fn write_exception_link_reg(addr: VirtualAddress) {
    core::arch::asm!("msr ELR_EL1, {v}", v = in(reg) addr.0);
}

/// Reads the stack pointer for exception level `el`.
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

/// Writes the stack pointer for exception level `el`.
///
/// # Safety
/// It is up to the caller to ensure that the pointer is valid to be stack pointer (i.e. the memory
/// is allocated and mapped correctly). It is also up to the caller to pass a value for `el` that
/// is valid considering the current value of `el`.
pub unsafe fn write_stack_pointer(el: u8, sp: VirtualAddress) {
    match el {
        0 => core::arch::asm!("msr SP_EL0, {v}", v = in(reg) sp.0),
        1 => core::arch::asm!("msr SP_EL1, {v}", v = in(reg) sp.0),
        2 => core::arch::asm!("msr SP_EL2, {v}", v = in(reg) sp.0),
        // 3 => core::arch::asm!("msr SP_EL3, {v}", v = in(reg) sp.0),
        _ => panic!("invalid exception level {el}"),
    }
}
