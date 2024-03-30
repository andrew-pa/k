//! This program runs as a replacement for `init` and runs API/integration tests against the kernel
//! from user space.
#![no_std]
#![no_main]
#![recursion_limit = "256"]
#![feature(int_roundings)]
#![feature(allocator_api)]
#![feature(custom_test_frameworks)]
#![feature(iter_array_chunks)]
#![feature(non_null_convenience)]

use core::ptr::NonNull;

use kapi::{
    commands::{Command, Kind as CmdKind, Test},
    completions::{self, Completion, ErrorCode, Kind as CplKind},
    queue::{Queue, FIRST_RECV_QUEUE_ID, FIRST_SEND_QUEUE_ID},
    system_calls::{exit, yield_now, KernelLogger},
    ProcessId,
};

pub trait Testable {
    /// Execute the test.
    fn run(&self, send_qu: &Queue<Command>, recv_qu: &Queue<Completion>);
}

impl<T> Testable for T
where
    T: Fn(&Queue<Command>, &Queue<Completion>),
{
    fn run(&self, send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
        log::debug!("running {}...", core::any::type_name::<T>());
        self(send_qu, recv_qu);
        log::info!("{} ok", core::any::type_name::<T>());
    }
}

pub fn test_runner(
    tests: &[&[&dyn Testable]],
    send_qu: &Queue<Command>,
    recv_qu: &Queue<Completion>,
) {
    log::info!("running {} test groups...", tests.len());
    for test_group in tests {
        log::info!("running {} tests...", test_group.len());
        for test in test_group.iter() {
            test.run(send_qu, recv_qu);
        }
    }
    log::info!("all tests successful");
}

fn cmd_invalid(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(&Command {
            id: 0,
            kind: CmdKind::Invalid,
        })
        .expect("post message to channel");

    loop {
        if let Some(c) = recv_qu.poll() {
            assert_eq!(c.response_to_id, 0);
            assert_eq!(c.kind, CplKind::Err(ErrorCode::UnknownCommand));
            return;
        }
        yield_now();
    }
}

fn cmd_test(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(&Command {
            id: 0,
            kind: Test { arg: 42 }.into(),
        })
        .expect("post message to channel");

    loop {
        if let Some(c) = recv_qu.poll() {
            assert_eq!(c.response_to_id, 0);
            assert_eq!(
                c.kind,
                completions::Test {
                    pid: ProcessId::new(1).unwrap(),
                    arg: 42,
                }
                .into()
            );
            return;
        }
        yield_now();
    }
}

mod queues;

#[no_mangle]
pub extern "C" fn _start(
    send_qu_addr: usize,
    send_qu_len: usize,
    recv_qu_addr: usize,
    recv_qu_len: usize,
) {
    log::set_logger(&KernelLogger).expect("set logger");
    log::set_max_level(log::LevelFilter::Trace);
    log::info!("starting API test process. kernel queues @ Send[0x{send_qu_addr:x}:{send_qu_len:x}] Recv[0x{recv_qu_addr:x}:{recv_qu_len:x}]");
    let (send_qu, recv_qu) = unsafe {
        (
            Queue::new(
                FIRST_SEND_QUEUE_ID,
                send_qu_len,
                NonNull::new(send_qu_addr as *mut _).expect("send queue ptr is non-null"),
            ),
            Queue::<Completion>::new(
                FIRST_RECV_QUEUE_ID,
                recv_qu_len,
                NonNull::new(recv_qu_addr as *mut _).expect("recv queue ptr is non-null"),
            ),
        )
    };

    test_runner(
        &[&[&cmd_invalid, &cmd_test], queues::TESTS],
        &send_qu,
        &recv_qu,
    );

    exit(0);
}

#[panic_handler]
pub fn panic_handler(info: &core::panic::PanicInfo) -> ! {
    log::error!("panic! {info}");
    exit(1)
}
