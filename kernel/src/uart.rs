//! Driver for PL011 UART that is provided by QEMU, implemented as a [log::Log].

use core::fmt::Write;

const DEBUG_UART_ADDRESS: usize = 0xffff_0000_0900_0000;

/// A very simple UART driver for debugging.
pub struct DebugUart {
    pub base: *mut u8,
}

impl Default for DebugUart {
    fn default() -> Self {
        Self {
            base: DEBUG_UART_ADDRESS as *mut u8,
        }
    }
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

fn color_for_level(lvl: log::Level) -> &'static str {
    match lvl {
        log::Level::Error => "31",
        log::Level::Warn => "33",
        log::Level::Info => "32",
        log::Level::Debug => "34",
        log::Level::Trace => "35",
    }
}

/// A logger that writes to the debug UART.
pub struct DebugUartLogger;

/// Modules that have Trace/Debug level logging disabled because they are very noisy.
/// All submodules will also be muted.
const DISABLED_MODULES: &[&str] = &[
    "kernel::memory",
    "kernel::process::thread::scheduler",
    "kernel::exception",
];

impl log::Log for DebugUartLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        if metadata.level() > log::Level::Info {
            DISABLED_MODULES
                .iter()
                .all(|m| !metadata.target().starts_with(m))
        } else {
            true
        }
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        //WARN: this is currently NOT thread safe!
        let mut uart = DebugUart::default();
        write!(
            uart,
            "[\x1b[{}m{:<5}\x1b[0m {} T",
            color_for_level(record.level()),
            record.level(),
            crate::timer::counter()
        )
        .unwrap();
        if let Some(tid) = crate::process::thread::scheduler::try_current_thread_id() {
            write!(uart, "{tid}").unwrap();
        } else {
            write!(uart, "?").unwrap();
        }
        writeln!(
            uart,
            " {}.{}] {}",
            record.module_path().unwrap_or("unknown module"),
            record.line().unwrap_or(0),
            record.args()
        )
        .unwrap();
    }

    fn flush(&self) {}
}
