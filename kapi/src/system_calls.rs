//! Definitions for synchronous system calls.
use core::arch::asm;

/// The numeric symbol given to identify each system call.
#[repr(u16)]
pub enum SystemCallNumber {
    Exit = 1,
    WriteLog = 4,
    Yield = 10,
    WaitForMessage = 11,
}

/// Exit the current process.
#[inline]
pub fn exit(code: u32) -> ! {
    unsafe {
        asm!(
            "mov w0, {p:w}",
            "svc #1",
            p = in(reg) code
        )
    }
    unreachable!()
}

/// Yield execution, causing a new thread to be scheduled.
#[inline]
pub fn yield_now() {
    unsafe { asm!("svc #10",) }
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
