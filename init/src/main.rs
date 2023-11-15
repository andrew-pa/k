#![no_std]
#![no_main]
#![recursion_limit = "256"]
#![feature(int_roundings)]
#![feature(allocator_api)]
#![feature(custom_test_frameworks)]
#![feature(iter_array_chunks)]

use core::arch::asm;

static TEST: Option<u64> = None;

fn fact(n: usize) -> usize {
    if n < 1 {
        1
    } else {
        n * fact(n - 1)
    }
}

#[no_mangle]
pub extern "C" fn _start() {
    // fact(5);
    unsafe {
        loop {
            asm!("svc #3")
        }
    }
}

#[panic_handler]
pub fn panic_handler(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
