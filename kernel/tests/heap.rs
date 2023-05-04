#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(kernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use kernel::*;

#[test_case]
fn test_allocate_trival() {
    let val: u64 = 0x1234_1234_1234_1234;
    let test = Box::new(val);
    assert_eq!(*test, val);
}

#[test_case]
fn test_allocate_huge() {
    let _: Vec<usize> = Vec::with_capacity(1_000_000);
}

#[test_case]
fn test_make_big_vec() {
    let mut v = Vec::new();
    for i in 0..10_000 {
        v.push(i * 3);
    }
}

#[test_case]
fn test_unaligned_sizes() {
    let _ = Box::new([7u8; 1]);
    let _ = Box::new([7u8; 2]);
    let _ = Box::new([7u8; 3]);
    let _ = Box::new([7u8; 4]);
    let _ = Box::new([7u8; 5]);
    let _ = Box::new([7u8; 6]);
    let _ = Box::new([7u8; 7]);
}

#[no_mangle]
pub extern "C" fn kmain() {
    // make sure the BSS section is zeroed (otherwise QEMUExit won't work!)
    unsafe {
        memory::zero_bss_section();
    }
    init::init_logging(log::LevelFilter::Trace);

    // do enough init to make the heap available
    unsafe {
        exception::install_exception_vector_table();
    }

    let dt = unsafe { dtb::DeviceTree::at_address(memory::VirtualAddress(0xffff_0000_4000_0000)) };
    memory::init_physical_memory_allocator(&dt);
    memory::paging::init_kernel_page_table();

    test_main();
}
