#![no_std]
#![no_main]
#![recursion_limit = "256"]
#![feature(int_roundings)]
#![feature(allocator_api)]
#![feature(custom_test_frameworks)]
#![feature(iter_array_chunks)]
#![feature(non_null_convenience)]

use core::arch::asm;

use kapi::Command;

mod channel;

#[inline]
fn log_record(r: &log::Record) {
    unsafe {
        asm!(
            "mov x0, {p}",
            "svc #4",
            p = in(reg) r as *const log::Record
        )
    }
}

#[inline]
fn exit() {
    unsafe { asm!("svc #1") }
}

pub struct KernelLogger;

impl log::Log for KernelLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        log_record(record);
    }

    fn flush(&self) {}
}

#[no_mangle]
pub extern "C" fn _start(
    channel_base_addr: usize,
    channel_sub_size: usize,
    channel_com_size: usize,
) {
    log::set_logger(&KernelLogger).expect("set logger");
    log::set_max_level(log::LevelFilter::Trace);
    log::info!("starting init process. kernel channel @ 0x{channel_base_addr:x}:{channel_sub_size:x}/{channel_com_size:x}");
    let mut ch = unsafe {
        channel::Channel::from_kernel(channel_base_addr, channel_sub_size, channel_com_size)
    };

    ch.post(&Command {
        kind: kapi::CommandKind::Test,
        id: 0,
        completion_semaphore: None,
        args: [1, 2, 3, 4],
    })
    .expect("post message to channel");

    log::info!("waiting for kernel response");

    loop {
        if let Some(c) = ch.poll() {
            log::info!("got kernel response: {c:?}");
            break;
        }
    }

    let sus_addr = 0x5000 as *mut u8;
    let x = unsafe { sus_addr.read_volatile() };
    log::info!("got {x}");

    // let path = "/fat/abcdefghij/test.txt";
    let path = "/fat/init";
    log::info!("spawning process {path}");
    ch.post(&Command {
        kind: kapi::CommandKind::SpawnProcess,
        id: 1,
        completion_semaphore: None,
        args: [path.as_ptr() as u64, path.len() as u64, 0, 0],
    })
    .expect("post message to channel");

    loop {
        if let Some(c) = ch.poll() {
            log::info!("got kernel response: {c:?}");
            break;
        }
    }

    exit();
}

#[panic_handler]
pub fn panic_handler(info: &core::panic::PanicInfo) -> ! {
    log::error!("panic! {info}");
    exit();
    loop {}
}
