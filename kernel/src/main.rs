#![no_std]
#![no_main]
#![recursion_limit = "256"]
#![feature(int_roundings)]
#![feature(lang_items)]
#![feature(allocator_api)]
#![feature(linked_list_cursors)]
#![feature(custom_test_frameworks)]
#![test_runner(kernel::test_runner)]
#![reexport_test_harness_main = "test_main"]
#![allow(unused)]

use core::{arch::global_asm, panic::PanicInfo};
use hashbrown::HashMap;
use smallvec::SmallVec;

use kernel::*;

#[no_mangle]
pub extern "C" fn kmain() {
    // make sure the BSS section is zeroed
    unsafe {
        memory::zero_bss_section();
    }
    init::init_logging(log::LevelFilter::Debug);

    let dt = unsafe { dtb::DeviceTree::at_address(0xffff_0000_4000_0000 as *mut u8) };

    // initialize virtual memory and interrupts
    unsafe {
        exception::install_exception_vector_table();
        memory::init_physical_memory_allocator(&dt);
        memory::paging::init_kernel_page_table();

        /* --- Kernel heap is now available --- */

        memory::init_virtual_address_allocator();
        exception::init_interrupts(&dt);
        tasks::init_executor();
        process::scheduler::init_scheduler(process::IDLE_THREAD);
    }

    log::info!("kernel systems initialized");

    // initialize PCIe bus and devices
    let mut pcie_drivers = HashMap::new();
    pcie_drivers.insert(
        0x01080200,
        storage::nvme::init_nvme_over_pcie as bus::pcie::DriverInitFn,
    );
    bus::pcie::init(&dt, &pcie_drivers);

    // create idle thread
    process::threads().insert(
        process::IDLE_THREAD,
        process::Thread::idle_thread()
    );

    // initialize system timer and interrupt
    init::configure_time_slicing(&dt);

    // enable all interrupts in DAIF process state mask
    exception::write_interrupt_mask(exception::InterruptMask::all_enabled());

    #[cfg(test)]
    tasks::spawn(async {
        test_main();
    });

    log::info!("running task executor...");
    tasks::run_executor()
}

/* TODO:
 *  + initialize MMU & provide API for page allocation and changing page tables. also make sure reserved regions on memory are correctly mapped
 *      - you can identity map the page the instruction ptr is in and then jump elsewhere safely
 *      - need to do initial mapping so that we can compile/link the kernel to run at high addresses
 *  + kernel heap/GlobalAlloc impl
 *  + set up interrupt handlers
 *  + start timer interrupt
 *  + switching between user/kernel space
 *  + process scheduling
 *  - system calls
 *  - message passing
 *  - shared memory
 *  - file system
 */
