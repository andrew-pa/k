#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(kernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

use kernel::*;

#[test_case]
fn test_v2m_works() {
    let mut ic = exception::interrupt_controller();
    assert!(ic.msi_supported());
    let msi = ic.alloc_msi().expect("allocate MSI");
    log::debug!("allocated msi {msi:?}");
    ic.set_target_cpu(msi.intid, 0x1);
    ic.set_priority(msi.intid, 0);
    ic.set_config(msi.intid, exception::InterruptConfig::Level);
    ic.set_pending(msi.intid, false);
    ic.set_enable(msi.intid, true);

    // attempt to trigger interrupt via CPU write
    log::debug!("writing to register to trigger interrupt");
    unsafe {
        let ptr: *mut u32 = msi.register_addr.to_virtual_canonical().as_ptr();
        ptr.write_volatile(msi.data_value);
    }

    loop {
        log::debug!("waiting for interrupt...");
        wait_for_interrupt();
    }
}

#[no_mangle]
pub extern "C" fn kmain() {
    // make sure the BSS section is zeroed (otherwise QEMUExit won't work!)
    unsafe {
        memory::zero_bss_section();
    }
    init::init_logging(log::LevelFilter::Trace);
    let dt = unsafe { dtb::DeviceTree::at_address(memory::VirtualAddress(0xffff_0000_4000_0000)) };

    // initialize virtual memory and interrupts
    unsafe {
        exception::install_exception_vector_table();
    }
    memory::init_physical_memory_allocator(&dt);
    memory::paging::init_kernel_page_table();
    memory::init_virtual_address_allocator();

    exception::init_interrupts(&dt);
    exception::write_interrupt_mask(exception::InterruptMask::all_enabled());

    test_main();
}
