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

use kernel::{registry::Path, *};

#[no_mangle]
pub extern "C" fn kmain() {
    // make sure the BSS section is zeroed
    unsafe {
        memory::zero_bss_section();
    }
    init::init_logging(log::LevelFilter::Trace);

    let dt = unsafe {
        dtb::DeviceTree::at_address(memory::PhysicalAddress(0x4000_0000).to_virtual_canonical())
    };

    // initialize virtual memory and interrupts
    unsafe {
        exception::install_exception_vector_table();
    }

    memory::init_physical_memory_allocator(&dt);
    memory::paging::init_kernel_page_table();

    /* --- Kernel heap is now available --- */

    memory::init_virtual_address_allocator();
    exception::init_interrupts(&dt);
    process::scheduler::init_scheduler(process::IDLE_THREAD);
    tasks::init_executor();
    registry::init_registry();

    log::info!("kernel systems initialized");

    // initialize PCIe bus and devices
    let mut pcie_drivers = HashMap::new();
    pcie_drivers.insert(
        0x01080200, // MassStorage:NVM:NVMe I/O controller
        storage::nvme::init_nvme_over_pcie as bus::pcie::DriverInitFn,
    );
    bus::pcie::init(&dt, &pcie_drivers);

    // initialize system timer and interrupt
    init::configure_time_slicing(&dt);

    /*tasks::spawn(async {
        let mut bs = registry::registry()
            .open_block_store(registry::Path::new("/dev/nvme/pci@0:2:0/1"))
            .await
            .unwrap();
        let s = bs.supported_block_size();
        log::info!("supported block size = {}", s);
        let mut buf = memory::PhysicalBuffer::alloc(1, &Default::default()).unwrap();
        buf.as_bytes_mut().fill(0);
        let res = bs
            .read_blocks(storage::LogicalAddress(0x3f), buf.physical_address(), 1)
            .await;
        log::info!("read result = {res:?}");
        log::debug!("data = {:x?}", &buf.as_bytes()[0..s]);
    });*/

    tasks::spawn(async {
        log::info!("open /dev/nvme/pci@0:2:0/1");
        let mut bs = registry::registry()
            .open_block_store(Path::new("/dev/nvme/pci@0:2:0/1"))
            .await
            .unwrap();
        log::info!("mount FAT filesystem");
        fs::fat::mount(Path::new("/fat"), bs).await.unwrap();
    });

    #[cfg(test)]
    tasks::spawn(async {
        test_main();
    });

    log::info!("creating task executor thread");
    let task_stack = memory::PhysicalBuffer::alloc(1024, &Default::default())
        .expect("allocate task exec thread stack");

    process::spawn_thread(process::Thread::kernel_thread(
        process::TASK_THREAD,
        tasks::run_executor,
        &task_stack,
    ));

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
