#![no_std]
#![no_main]
#![recursion_limit = "256"]
#![feature(int_roundings)]
#![feature(allocator_api)]
#![feature(linked_list_cursors)]
#![feature(custom_test_frameworks)]
#![feature(iter_array_chunks)]
#![feature(non_null_convenience)]
#![test_runner(kernel::test_runner)]
#![reexport_test_harness_main = "test_main"]
#![allow(unused)]

use core::{arch::global_asm, panic::PanicInfo};
use smallvec::SmallVec;

use kernel::{registry::Path, *};

extern crate alloc;

#[no_mangle]
pub extern "C" fn kmain() {
    unsafe {
        memory::zero_bss_section();
    }

    init::logging(log::LevelFilter::Trace);

    if read_current_el() != 1 {
        todo!("switch from {} to EL1 at boot", read_current_el());
    }

    // load the device tree blob that u-boot places at 0x4000_0000 when it loads the kernel
    let dt = unsafe {
        dtb::DeviceTree::at_address(memory::PhysicalAddress(0x4000_0000).to_virtual_canonical())
    };

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

    tasks::spawn(init::mount_root_fs_and_spawn_init());

    #[cfg(test)]
    tasks::spawn(async {
        test_main();
    });

    init::spawn_task_executor_thread();

    log::trace!("enabling interrupts");
    unsafe {
        exception::write_interrupt_mask(exception::InterruptMask::all_enabled());
    }

    // this loop becomes the idle thread, see [process::thread::scheduler::ThreadScheduler::new()].
    log::trace!("idle loop starting");
    loop {
        log::trace!("idle top of loop");
        wait_for_interrupt()
    }
}
