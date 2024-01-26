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
use hashbrown::HashMap;
use smallvec::SmallVec;

use kernel::{registry::Path, *};

extern crate alloc;

#[no_mangle]
pub extern "C" fn kmain() {
    unsafe {
        memory::zero_bss_section();
    }

    init::init_logging(log::LevelFilter::Trace);

    // load the device tree blob that u-boot places at 0x4000_0000 when it loads the kernel
    let dt = unsafe {
        dtb::DeviceTree::at_address(memory::PhysicalAddress(0x4000_0000).to_virtual_canonical())
    };

    unsafe {
        exception::install_exception_vector_table();
    }

    // setup memory management
    memory::init_physical_memory_allocator(&dt);
    memory::paging::init_kernel_page_table();

    /* --- Kernel heap is now available --- */

    memory::init_virtual_address_allocator();
    exception::init_interrupts(&dt);
    tasks::init_executor();
    registry::init_registry();
    process::thread::scheduler::init_scheduler(process::thread::IDLE_THREAD);

    log::info!("kernel systems initialized");

    // initialize PCIe bus and devices
    let mut pcie_drivers = HashMap::new();
    pcie_drivers.insert(
        0x01080200, // MassStorage:NVM:NVMe I/O controller
        storage::nvme::init_nvme_over_pcie as bus::pcie::DriverInitFn,
    );
    bus::pcie::init(&dt, &pcie_drivers);

    init::configure_time_slicing(&dt);

    init::register_system_call_handlers();

    // mount root filesystem and start init process
    tasks::spawn(async {
        log::info!("open /dev/nvme/pci@0:2:0/1");
        let mut bs = {
            registry::registry()
                .open_block_store(Path::new("/dev/nvme/pci@0:2:0/1"))
                .await
                .unwrap()
        };
        log::info!("mount FAT filesystem");
        fs::fat::mount(Path::new("/fat"), bs).await.unwrap();

        let test_file = registry::registry()
            .open_file(Path::new("/fat/abcdefghij/test.txt"))
            .await
            .expect("open file");

        log::info!("spawning init process");
        let init_pid = process::spawn_process(
            "/fat/init",
            Some(|proc: &mut process::Process| {
                proc.attach_file(test_file).unwrap();
            }),
        )
        .await
        .expect("spawn init process");

        log::info!("init pid = {init_pid}");
    });

    #[cfg(test)]
    tasks::spawn(async {
        test_main();
    });

    init::spawn_task_executor_thread();

    // enable all interrupts in DAIF process state mask
    log::trace!("enabling interrupts");
    exception::write_interrupt_mask(exception::InterruptMask::all_enabled());

    log::trace!("idle loop starting");
    loop {
        log::debug!("idle top of loop");
        wait_for_interrupt()
    }
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
