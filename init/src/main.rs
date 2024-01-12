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
    let sus_addr = 0x1002 as *mut u8;
    let x = unsafe { sus_addr.read_volatile() };
    unsafe {
        let mut i: usize = x as usize;
        loop {
            i = i.wrapping_add(1);
            asm!(
                "mov x0, {i}",
                "svc #3",
                i = in(reg) i
            )
        }
    }
}

#[panic_handler]
pub fn panic_handler(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
