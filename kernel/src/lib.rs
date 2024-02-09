//! The kernel for the ??? operating system.
//!
//! The kernel has a modular, but monolithic design.
//! Execution starts in `src/main.rs`, in the `kmain` function.
//! Devices are detected using the Device Tree blob provided by `u-boot` (see [dtb]).
//! The kernel uses `async`/`await` to handle asynchronous operations, and includes a kernel-level
//! task executor to drive tasks to completion (see [tasks]). This runs in its own kernel thread.
#![no_std]
#![no_main]
#![recursion_limit = "256"]
#![feature(int_roundings)]
#![feature(allocator_api)]
#![feature(linked_list_cursors)]
#![feature(error_in_core)]
#![feature(iter_array_chunks)]
#![feature(custom_test_frameworks)]
#![feature(non_null_convenience)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]
#![allow(unused)]

extern crate alloc;

pub mod dtb;
pub mod registry;

pub mod exception;
pub mod memory;
pub mod process;
pub mod tasks;

pub mod bus;
pub mod storage;
pub mod timer;
pub mod uart;

pub mod fs;

pub mod init;

use core::{arch::global_asm, panic::PanicInfo};

global_asm!(include_str!("start.S"));

/// A concurrent hash map using spinlocks.
pub type CHashMapG<K, V> =
    chashmap::CHashMap<K, V, hashbrown::hash_map::DefaultHashBuilder, spin::RwLock<()>>;
/// The read guard for [CHashMapG].
pub type CHashMapGReadGuard<'a, K, V> =
    chashmap::ReadGuard<'a, K, V, hashbrown::hash_map::DefaultHashBuilder, spin::RwLock<()>>;
/// The write guard for [CHashMapG].
pub type CHashMapGWriteGuard<'a, K, V> =
    chashmap::WriteGuard<'a, K, V, hashbrown::hash_map::DefaultHashBuilder, spin::RwLock<()>>;

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

/// Handle panics in the kernel by writing them to the debug UART.
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
    init::logging(log::LevelFilter::Trace);

    // initialize virtual memory and exceptions
    unsafe {
        exception::install_exception_vector_table();
    }
    let dt = unsafe { dtb::DeviceTree::at_address(memory::VirtualAddress(0xffff_0000_4000_0000)) };
    memory::init_physical_memory_allocator(&dt);
    memory::paging::init_kernel_page_table();
    memory::init_virtual_address_allocator();

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
        writeln!(&mut uart, "ok");
    }
}

/// Run provided tests, then exit QEMU or halt.
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
