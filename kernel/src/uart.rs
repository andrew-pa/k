// driver for PL011 UART that is provided by QEMU
// for debugging, of course

use core::fmt::Write;

pub struct DebugUart {
    pub base: *mut u8,
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

pub struct DebugUartLogger;

impl log::Log for DebugUartLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        //WARN: this is currently NOT thread safe!
        let mut uart = DebugUart {
            base: 0xffff_0000_0900_0000 as *mut u8,
        };
        writeln!(
            uart,
            "[{:<5} {} {}.{}] {}",
            record.level(),
            crate::timer::counter(),
            record.module_path().unwrap_or("unknown module"),
            record.line().unwrap_or(0),
            record.args()
        )
        .unwrap();
    }

    fn flush(&self) {}
}
