#![no_std]
#![no_main]
#![recursion_limit = "256"]
#![feature(int_roundings)]
#![feature(lang_items)]
#![feature(allocator_api)]
#![feature(linked_list_cursors)]
#![feature(type_name_of_val)]
#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]
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

pub mod init;

use core::{arch::global_asm, panic::PanicInfo};

global_asm!(include_str!("start.S"));

pub type CHashMapG<K, V> =
    chashmap::CHashMap<K, V, hashbrown::hash_map::DefaultHashBuilder, spin::RwLock<()>>;
pub type CHashMapGReadGuard<'a, K, V> =
    chashmap::ReadGuard<'a, K, V, hashbrown::hash_map::DefaultHashBuilder, spin::RwLock<()>>;
pub type CHashMapGWriteGuard<'a, K, V> =
    chashmap::WriteGuard<'a, K, V, hashbrown::hash_map::DefaultHashBuilder, spin::RwLock<()>>;

#[inline]
pub fn wait_for_interrupt() {
    unsafe { core::arch::asm!("wfi", options(nomem, nostack)) }
}

/// disable interrupts and loop forever
pub fn halt() -> ! {
    exception::write_interrupt_mask(exception::InterruptMask(1));
    timer::set_enabled(false);
    timer::set_interrupts_enabled(false);
    loop {
        wait_for_interrupt();
    }
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

fn print_panic(info: &core::panic::PanicInfo) {
    use core::fmt::Write;
    let mut uart = uart::DebugUart {
        base: 0xffff_0000_0900_0000 as *mut u8,
    };
    let _ = uart.write_fmt(format_args!("\npanic! {info}\n"));
}

#[panic_handler]
pub fn panic_handler(info: &core::panic::PanicInfo) -> ! {
    use core::fmt::Write;
    let mut uart = uart::DebugUart {
        base: 0xffff_0000_0900_0000 as *mut u8,
    };
    let _ = uart.write_fmt(format_args!("\npanic! {info}\n"));
    // TODO: why can't we get this to only happen during cfg(test)?
    use qemu_exit::QEMUExit;
    qemu_exit::aarch64::AArch64::new().exit_failure();
    // prevent anything from getting scheduled after a kernel panic
    halt();
}

#[cfg(test)]
#[no_mangle]
pub extern "C" fn kmain() {
    // make sure the BSS section is zeroed (otherwise QEMUExit won't work!)
    unsafe {
        memory::zero_bss_section();
    }
    init::init_logging(log::LevelFilter::Trace);
    test_main();
}

pub trait Testable {
    fn run(&self);
}

impl<T> Testable for T
where
    T: Fn(),
{
    fn run(&self) {
        use core::fmt::Write;
        let mut uart = uart::DebugUart {
            base: 0xffff_0000_0900_0000 as *mut u8,
        };
        write!(&mut uart, "{}...\t", core::any::type_name::<T>());
        self();
        write!(&mut uart, "ok\n");
    }
}

pub fn test_runner(tests: &[&dyn Testable]) {
    log::info!("running {} tests...", tests.len());
    for test in tests {
        test.run();
    }
    log::info!("all tests successful");
    use qemu_exit::QEMUExit;
    qemu_exit::aarch64::AArch64::new().exit_success();
    halt();
}
