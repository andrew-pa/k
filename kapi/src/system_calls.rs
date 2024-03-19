//! Definitions for synchronous system calls.

/// The numeric symbol given to identify each system call.
#[repr(u16)]
pub enum SystemCallNumber {
    Exit = 1,
    WriteLog = 4,
    Yield = 10,
    WaitForMessage = 11,
}

#[cfg(not(features = "kernel"))]
mod wrappers {
    use core::arch::asm;

    /// Exit the current process.
    #[inline]
    pub fn exit() {
        unsafe { asm!("svc #1") }
    }

    /// Write a log record into the system log.
    #[inline]
    pub fn log_record(r: &log::Record) {
        unsafe {
            asm!(
                "mov x0, {p}",
                "svc #4",
                p = in(reg) r as *const log::Record
            )
        }
    }

    /// A [log::Log] implementation that writes log records into the system log.
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
}

#[cfg(not(features = "kernel"))]
pub use wrappers::*;
