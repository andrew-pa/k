#![no_std]
#![no_main]

use kapi::system_calls::{current_process_id, current_thread_id, exit, yield_now, KernelLogger};

fn fib(n: usize) -> usize {
    if n < 2 {
        1
    } else {
        fib(n - 1) + fib(n - 2)
    }
}

#[no_mangle]
pub extern "C" fn _start(
    _send_qu_addr: usize,
    _send_qu_cap: usize,
    _recv_qu_addr: usize,
    _recv_qu_cap: usize,
    params_addr: usize,
    params_size: usize,
) {
    log::set_logger(&KernelLogger).expect("set logger");
    log::set_max_level(log::LevelFilter::Trace);
    log::info!("api_tests test process. params @ 0x{params_addr:x}:{params_size}");

    log::debug!(
        "process id = {}, thread id = {}",
        current_process_id(),
        current_thread_id()
    );

    // make sure that the stack is setup correctly
    assert_eq!(fib(10), 89);

    if params_size > 0 {
        let p = params_addr as *const u8;
        log::info!("got param: {}, hanging", unsafe { p.read() });

        loop {
            yield_now();
        }
    }

    exit(0);
}

#[panic_handler]
pub fn panic_handler(info: &core::panic::PanicInfo) -> ! {
    log::error!("panic! {info}");
    exit(1)
}
