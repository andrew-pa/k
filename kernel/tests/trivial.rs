#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(kernel::test_runner)]

use core::arch::global_asm;

global_asm!(include_str!("../src/start.S"));

#[no_mangle]
pub extern "C" fn kmain() {}
