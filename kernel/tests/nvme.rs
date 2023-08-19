#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(kernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

use hashbrown::HashMap;

use kernel::{storage::BlockAddress, *};

#[test_case]
fn write_then_read_scratch() {
    tasks::block_on(async {
        let mut bs = registry::registry()
            .open_block_store(registry::Path::new("/dev/nvme/pci@0:3:0/1"))
            .await
            .unwrap();
        log::info!("supported block size = {}", bs.supported_block_size());
        let mut buf = memory::PhysicalBuffer::alloc(1, &Default::default()).unwrap();
        buf.as_bytes_mut().fill(0);
        let data = "Hello, block storage!".as_bytes();
        buf.as_bytes_mut()[0..data.len()].copy_from_slice(data);
        bs.write_blocks(buf.physical_address(), BlockAddress(0), 1)
            .await
            .expect("write block");
        buf.as_bytes_mut()[0..data.len()].fill(0);
        bs.read_blocks(BlockAddress(0), buf.physical_address(), 1)
            .await
            .expect("read block");
        assert_eq!(&buf.as_bytes()[0..data.len()], data);
    })
}

#[no_mangle]
pub extern "C" fn kmain() {
    // make sure the BSS section is zeroed (otherwise QEMUExit won't work!)
    unsafe {
        memory::zero_bss_section();
    }
    init::init_logging(log::LevelFilter::Debug);
    let dt = unsafe { dtb::DeviceTree::at_address(memory::VirtualAddress(0xffff_0000_4000_0000)) };

    // initialize virtual memory and interrupts
    unsafe {
        exception::install_exception_vector_table();
    }
    memory::init_physical_memory_allocator(&dt);
    memory::paging::init_kernel_page_table();
    memory::init_virtual_address_allocator();

    exception::init_interrupts(&dt);
    process::scheduler::init_scheduler(process::IDLE_THREAD);

    registry::init_registry();

    // initialize PCIe bus and devices
    let mut pcie_drivers = HashMap::new();
    pcie_drivers.insert(
        0x01080200, // MassStorage:NVM:NVMe I/O controller
        storage::nvme::init_nvme_over_pcie as bus::pcie::DriverInitFn,
    );
    bus::pcie::init(&dt, &pcie_drivers);

    exception::write_interrupt_mask(exception::InterruptMask::all_enabled());

    test_main();
}
