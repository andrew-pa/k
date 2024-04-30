//! The init process is the first user space process spawned after boot and is responsible for setting up and managing user space.
#![no_std]
#![no_main]
#![recursion_limit = "256"]
#![feature(int_roundings)]
#![feature(allocator_api)]
#![feature(custom_test_frameworks)]
#![feature(iter_array_chunks)]

use core::ptr::NonNull;

use kapi::{
    commands::{Command, Test},
    completions::Completion,
    queue::{Queue, FIRST_RECV_QUEUE_ID, FIRST_SEND_QUEUE_ID},
    system_calls::{exit, KernelLogger},
};

#[no_mangle]
pub extern "C" fn _start(
    send_qu_addr: usize,
    send_qu_cap: usize,
    recv_qu_addr: usize,
    recv_qu_cap: usize,
) {
    log::set_logger(&KernelLogger).expect("set logger");
    log::set_max_level(log::LevelFilter::Trace);
    log::info!("starting init process. kernel queues @ Send[0x{send_qu_addr:x}:{send_qu_cap:x}] Recv[0x{recv_qu_addr:x}:{recv_qu_cap:x}]");
    let (send_qu, recv_qu) = unsafe {
        (
            Queue::new(
                FIRST_SEND_QUEUE_ID,
                NonNull::new(send_qu_addr as *mut _).expect("send queue ptr is non-null"),
                send_qu_cap,
            ),
            Queue::<Completion>::new(
                FIRST_RECV_QUEUE_ID,
                NonNull::new(recv_qu_addr as *mut _).expect("recv queue ptr is non-null"),
                recv_qu_cap,
            ),
        )
    };

    send_qu
        .post(&Command {
            id: 0,
            kind: Test { arg: 42 }.into(),
        })
        .expect("post message to channel");

    log::info!("waiting for kernel response");

    loop {
        if let Some(c) = recv_qu.poll() {
            log::info!("got kernel response: {c:?}");
            break;
        }
    }

    exit(0)
}

#[panic_handler]
pub fn panic_handler(info: &core::panic::PanicInfo) -> ! {
    log::error!("panic! {info}");
    exit(1)
}
