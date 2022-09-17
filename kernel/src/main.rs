#![feature(lang_items)]
#![no_std]
#![no_main]

use core::{panic::PanicInfo, fmt::Write};

mod dtb;
mod uart;

struct DebugUart {
    base: *mut u8
}

impl Write for DebugUart {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for b in s.bytes() {
            unsafe {
                self.base.write_volatile(b);
            }
        }
        Ok(())
    }
}

struct DebugUartLogger;

impl log::Log for DebugUartLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        //WARN: this is currently NOT thread safe!
        let mut uart = DebugUart { base: 0x09000000 as *mut u8 };
        writeln!(uart, "[{} {}] {} ({} ln {})",
            record.level(),
            record.module_path().unwrap_or("unknown module"),
            record.args(),
            record.file().unwrap_or("unknown file"),
            record.line().unwrap_or(0)).unwrap();
    }

    fn flush(&self) { }
}

#[inline]
pub fn halt() -> ! {
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack))
        }
    }
}

#[inline(never)]
pub extern fn do_something_else() {
    let uart = 0x09000000 as *mut u8;
    unsafe { uart.write_volatile(b'x'); }
}

#[no_mangle]
pub extern fn kmain() {
    // log::set_logger(&DebugUartLogger).unwrap();
    /*log::info!("starting kernel");
    loop {
        log::trace!("...");
    }*/
    let mut uart = DebugUart { base: 0x09000000 as *mut u8 };
    uart.write_fmt(format_args!("hello, fmt!\n")).unwrap();
    // writeln!(&mut uart, "Hello, world!").unwrap();
    // do_something_else();
    let uart = 0x09000000 as *mut u8;
    for _ in 0..10 {
        unsafe { uart.write_volatile(b'K'); }
    }
    halt();
}

#[panic_handler]
fn panic_handler(info: &PanicInfo) -> ! {
    let mut uart = DebugUart { base: 0x09000000 as *mut u8 };
    uart.write_str("panic!\n");
    // log::error!("panic: {info}");
    halt();
}
