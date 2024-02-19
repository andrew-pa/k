//! The kernel for the ??? operating system.
//!
//! The kernel has a modular, but monolithic design.
//! Execution starts in `start.S`, which then calls [kmain].
//! Devices are detected using the Device Tree blob provided by `u-boot` (see [dtb]).
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
#![feature(non_null_convenience)]
#![feature(error_in_core)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]
#![allow(unused)]
#![warn(missing_docs)]

extern crate alloc;

use core::{arch::global_asm, panic::PanicInfo};
use memory::PhysicalAddress;
use smallvec::SmallVec;

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

pub mod intrinsics;

/// A concurrent hash map using spinlocks.
pub type CHashMapG<K, V> =
    chashmap::CHashMap<K, V, hashbrown::hash_map::DefaultHashBuilder, spin::RwLock<()>>;
/// The read guard for [CHashMapG].
pub type CHashMapGReadGuard<'a, K, V> =
    chashmap::ReadGuard<'a, K, V, hashbrown::hash_map::DefaultHashBuilder, spin::RwLock<()>>;
/// The write guard for [CHashMapG].
pub type CHashMapGWriteGuard<'a, K, V> =
    chashmap::WriteGuard<'a, K, V, hashbrown::hash_map::DefaultHashBuilder, spin::RwLock<()>>;

global_asm!(include_str!("start.S"));

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

    // Load the device tree blob at the address provided by u-boot as a parameter.
    // See u-boot/arch/arm/lib/bootm.c:boot_jump_linux(...).
    log::debug!("reading device tree blob at {dtb_addr}");
    let dt = unsafe { dtb::DeviceTree::at_address(dtb_addr.to_virtual_canonical()) };

    dt.process_properties_for_node("chosen", |prop, data, _| match prop {
        "bootargs" => {
            let s = core::ffi::CStr::from_bytes_until_nul(data)
                .unwrap()
                .to_str()
                .unwrap();
            log::info!("bootargs = {s}");
        }
        _ => {}
    });

    unsafe {
        exception::install_exception_vector_table();
    }

    memory::init_physical_memory_allocator(&dt);
    memory::paging::init_kernel_page_table();

    /* --- Kernel heap is now available --- */

    memory::init_virtual_address_allocator();
    exception::init_interrupts(&dt);
    process::thread::scheduler::init_scheduler();
    tasks::init_executor();
    registry::init_registry();

    log::info!("kernel systems initialized");

    init::pcie(&dt);

    init::configure_time_slicing(&dt);
    init::register_system_call_handlers();

    #[cfg(test)]
    {
        // there are no tests for the kernel executable but Cargo still builds the it for test
        // mode.
        log::info!("boot succesful, running unit tests!");
        test_main();
    }

    tasks::spawn(init::mount_root_fs_and_spawn_init());

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
    intrinsics::halt();
}
