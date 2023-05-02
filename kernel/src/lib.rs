#![no_std]
#![no_main]
#![recursion_limit = "256"]
#![feature(int_roundings)]
#![feature(lang_items)]
#![feature(allocator_api)]
#![feature(linked_list_cursors)]
#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner)]
#![allow(unused)]

extern crate alloc;

pub mod dtb;

pub mod exception;
pub mod memory;
pub mod process;
pub mod tasks;

pub mod bus;
pub mod storage;
pub mod timer;
pub mod uart;

pub type CHashMapG<K, V> =
    chashmap::CHashMap<K, V, hashbrown::hash_map::DefaultHashBuilder, spin::RwLock<()>>;
pub type CHashMapGReadGuard<'a, K, V> =
    chashmap::ReadGuard<'a, K, V, hashbrown::hash_map::DefaultHashBuilder, spin::RwLock<()>>;
pub type CHashMapGWriteGuard<'a, K, V> =
    chashmap::WriteGuard<'a, K, V, hashbrown::hash_map::DefaultHashBuilder, spin::RwLock<()>>;

pub fn test_runner(tests: &[&dyn Fn()]) {
    if tests.len() > 0 {
        panic!("unit tests in directly in kernel are left unimplemented due to initialization ambiguity, use integration tests instead")
    }
}

#[inline]
pub fn wait_for_interrupt() {
    unsafe { core::arch::asm!("wfi", options(nomem, nostack)) }
}

pub fn current_el() -> usize {
    let mut current_el: usize;
    unsafe {
        core::arch::asm!(
            "mrs {val}, CurrentEL",
            val = out(reg) current_el
        );
    }
    current_el >> 2
}

pub fn mair() -> usize {
    let mut x: usize;
    unsafe {
        core::arch::asm!(
            "mrs {val}, MAIR_EL1",
            val = out(reg) x
        );
    }
    x >> 2
}

#[panic_handler]
pub fn panic_handler(info: &core::panic::PanicInfo) -> ! {
    use core::fmt::Write;
    let mut uart = uart::DebugUart {
        base: 0xffff_0000_0900_0000 as *mut u8,
    };
    let _ = uart.write_fmt(format_args!("\npanic! {info}\n"));
    // prevent anything from getting scheduled after a kernel panic
    exception::write_interrupt_mask(exception::InterruptMask(1));
    timer::set_enabled(false);
    timer::set_interrupts_enabled(false);
    loop {
        wait_for_interrupt();
    }
}
