//! Miscellaneous CPU intrinsic functions and register accessors.

use crate::exception;

/// Wait for an interrupt to occur. The function returns after an interrupt is triggered.
///
/// This uses the `wfi` instruction once.
#[inline]
pub fn wait_for_interrupt() {
    unsafe { core::arch::asm!("wfi", options(nomem, nostack)) }
}

/// Disable interrupts and loop forever, preventing any further execution.
pub fn halt() -> ! {
    unsafe {
        exception::write_interrupt_mask(exception::InterruptMask::all_disabled());
    }
    loop {
        wait_for_interrupt();
    }
}

/// Read the current exception level.
pub fn read_current_el() -> usize {
    let mut current_el: usize;
    unsafe {
        core::arch::asm!(
            "mrs {val}, CurrentEL",
            val = out(reg) current_el
        );
    }
    current_el >> 2
}

/// Read the MAIR register.
pub fn read_mair() -> usize {
    let mut x: usize;
    unsafe {
        core::arch::asm!(
            "mrs {val}, MAIR_EL1",
            val = out(reg) x
        );
    }
    x >> 2
}

/// Read the stack pointer select register.
pub fn read_sp_sel() -> bool {
    let mut v: usize;
    unsafe {
        core::arch::asm!(
            "mrs {v}, SPSel",
            v = out(reg) v
        );
    }
    v == 1
}
