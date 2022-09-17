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
        writeln!(uart, "[{:<5} {}] ({}:{}) {}",
            record.level(),
            record.module_path().unwrap_or("unknown module"),
            record.file().unwrap_or("unknown file"),
            record.line().unwrap_or(0),
            record.args()
        ).unwrap();
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

extern "C" {
    pub static mut __bss_start: u8;
    pub static mut __bss_end: u8;
}

#[no_mangle]
pub extern fn kmain() {
    unsafe {
        let bss_start = &mut __bss_start as *mut u8;
        let bss_end   = &mut __bss_end as *mut u8;
        let bss_size = bss_end.offset_from(bss_start) as usize;
        core::ptr::write_bytes(bss_start, 0, bss_size);
    }
    log::set_logger(&DebugUartLogger).expect("set logger");
    log::set_max_level(log::LevelFilter::Trace);
    log::info!("starting kernel");
    for i in 0..10 {
        log::trace!("... {i}");
    }
    halt();
}

#[panic_handler]
fn panic_handler(info: &PanicInfo) -> ! {
    let mut uart = DebugUart { base: 0x09000000 as *mut u8 };
    let _ = uart.write_fmt(format_args!("panic! {info}"));
    halt();
}
