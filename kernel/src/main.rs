//! The kernel for the ??? operating system.
//!
//! The kernel has a modular, but monolithic design.
//! Execution starts in `start.S`, which then calls [kmain].
//! Devices are detected using the Device Tree blob provided by `u-boot` (see [ds::dtb]).
//! The kernel uses `async`/`await` to handle asynchronous operations, and includes a kernel-level
//! task executor to drive tasks to completion (see [tasks]). This runs in its own kernel thread.
#![no_std]
#![no_main]
#![recursion_limit = "256"]
#![feature(int_roundings)]
#![feature(allocator_api)]
#![feature(linked_list_cursors)]
#![feature(custom_test_frameworks)]
#![feature(iter_array_chunks)]
#![feature(error_in_core)]
#![feature(str_from_raw_parts)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::boxed::Box;
use byteorder::ByteOrder;
use memory::PhysicalAddress;
use qemu_exit::QEMUExit as _;

use crate::platform::intrinsics::{self, current_cpu_id};

pub mod ds;
pub mod error;
pub mod platform;
pub mod registry;

pub mod exception;
pub mod memory;
pub mod process;
pub mod tasks;

pub mod bus;
pub mod storage;

pub mod fs;

pub mod init;

extern "C" {
    fn _secondary_start();
}

#[no_mangle]
pub extern "C" fn secondary_entry_point(context_id: usize) -> ! {
    unsafe {
        exception::install_exception_vector_table();
    }
    log::info!(
        "secondary CPU #{} start (context_id = {context_id:x})",
        current_cpu_id()
    );
    intrinsics::halt();
}

/// The main entry point for the kernel.
///
/// This function is called by `start.S` to boot the kernel.
/// The boot process initializes various kernel subsystems in order, then spawns the `init` process.
///
/// This function never returns, but instead becomes the idle thread loop.
#[no_mangle]
pub extern "C" fn kmain(dtb_addr: PhysicalAddress) -> ! {
    unsafe {
        memory::zero_bss_section();
    }

    init::logging(log::LevelFilter::Trace);

    if intrinsics::read_current_el() != 1 {
        todo!(
            "switch from {} to EL1 at boot",
            intrinsics::read_current_el()
        );
    }

    // set up our exception handlers as soon as possible so we can detect kernel errors.
    unsafe {
        exception::install_exception_vector_table();
    }

    // Load the device tree blob at the address provided by u-boot as a parameter.
    // See u-boot/arch/arm/lib/bootm.c:boot_jump_linux(...).
    log::trace!("reading device tree blob at {dtb_addr}");
    let dt = unsafe { ds::dtb::DeviceTree::at_address(dtb_addr.to_virtual_canonical()) };
    dt.log();

    let mut x: usize;
    unsafe {
        core::arch::asm!(
            "mrs {val}, CPACR_EL1",
            val = out(reg) x
        );
    }
    log::trace!("CPACR={:b}", x >> 15);

    memory::init_physical_memory_allocator(&dt);
    memory::paging::init_kernel_page_table();

    /* --- Kernel heap is now available --- */

    memory::init_virtual_address_allocator();
    exception::init_interrupts(&dt);
    init::smp_start(&dt);
    process::thread::scheduler::init_scheduler();
    tasks::init_executor();
    registry::init_registry();

    init::configure_time_slicing(&dt);
    init::register_system_call_handlers();

    log::info!("kernel systems initialized");

    init::pcie(&dt);

    let dt = Box::leak(Box::new(dt));
    #[allow(unused)] // the test block confuses the analyzer
    let opts = init::find_boot_options(dt);

    #[cfg(test)]
    #[allow(unreachable_code)]
    {
        log::info!("boot succesful, running unit tests!");
        test_main();
        qemu_exit::aarch64::AArch64::new().exit_success();
        intrinsics::halt();
    }

    // Spawn a task to finish booting the system. This task won't actually run until after we
    // enable interrupts and the scheduler schedules the task executor.
    tasks::spawn(init::finish_boot(opts));

    init::spawn_task_executor_thread();

    log::trace!("enabling interrupts");
    unsafe {
        exception::write_interrupt_mask(exception::InterruptMask::all_enabled());
    }

    // this loop becomes the idle thread, see [process::thread::scheduler::ThreadScheduler::new()].
    log::trace!("idle loop starting");
    loop {
        log::trace!("idle top of loop");
        intrinsics::wait_for_interrupt()
    }
}

/// Handle panics in the kernel by writing them to the debug UART.
#[panic_handler]
#[allow(unreachable_code)]
pub fn panic_handler(info: &core::panic::PanicInfo) -> ! {
    use core::fmt::Write;
    let cpu_id = intrinsics::current_cpu_id();
    let mut uart = platform::uart::DebugUart::default();
    let _ = writeln!(
        &mut uart,
        "\n\x1b[91;1mpanic on cpu #{cpu_id}!\x1b[0m {info}"
    );
    qemu_exit::aarch64::AArch64::new().exit_failure();
    // prevent anything from getting scheduled after a kernel panic
    intrinsics::halt();
}

/// Trait for custom test runner.
pub trait Testable {
    /// Execute the test.
    fn run(&self);
}

impl<T> Testable for T
where
    T: Fn(),
{
    fn run(&self) {
        use core::fmt::Write;
        let mut uart = platform::uart::DebugUart::default();
        write!(&mut uart, "{}...\t", core::any::type_name::<T>()).unwrap();
        self();
        writeln!(&mut uart, "ok").unwrap();
    }
}

/// Run tests as gathered by the test runner.
pub fn test_runner(tests: &[&dyn Testable]) {
    log::info!("running {} tests...", tests.len());
    for test in tests {
        test.run();
    }
    log::info!("all tests successful");
}

#[macro_export]
/// Assert that some type `T` implements [Send], causing a compiler error if not.
macro_rules! assert_send {
    ($t:ty) => {
        const _: fn() = || {
            struct CheckSend<T: Send>(core::marker::PhantomData<T>);
            let _ = CheckSend::<$t>(core::marker::PhantomData);
            unreachable!();
        };
    };
}

#[macro_export]
/// Assert that some type `T` implements [Sync], causing a compiler error if not.
macro_rules! assert_sync {
    ($t:ty) => {
        const _: fn() = || {
            struct CheckSync<T: Sync>(core::marker::PhantomData<T>);
            let _ = CheckSync::<$t>(core::marker::PhantomData);
            unreachable!();
        };
    };
}
