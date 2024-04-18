//! Definitions for synchronous system calls.
use core::ptr::addr_of_mut;
use core::{arch::asm, ptr::null_mut};

use crate::{ProcessId, ThreadId};

/// The numeric symbol given to identify each system call.
///
/// See the corresponding function wrapper for additional documentation.
#[repr(u16)]
#[allow(missing_docs)]
pub enum SystemCallNumber {
    Exit = 1,
    GetCurrentProcessId = 2,
    GetCurrentThreadId = 3,
    WriteLog = 4,

    Yield = 10,
    WaitForMessage = 11,

    HeapAllocate = 20,
    HeapFree = 21,
}

/// Exit the current thread immediately.
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

/// Get the current process ID.
#[inline]
pub fn current_process_id() -> ProcessId {
    unsafe {
        let mut pid: u32 = 0;
        asm!(
            "mov x0, {pid}",
            "svc #2",
            pid = in(reg) addr_of_mut!(pid)
        );
        // the kernel will return a valid PID
        ProcessId::new_unchecked(pid)
    }
}

/// Get the current thread ID.
#[inline]
pub fn current_thread_id() -> ThreadId {
    unsafe {
        let mut tid: u32 = 0;
        asm!(
            "mov x0, {tid}",
            "svc #3",
            tid = in(reg) addr_of_mut!(tid)
        );
        tid
    }
}

/// Allocate more memory in the process address space. The address will be page aligned, and the
/// size will be rounded up to the next page. The valid pointer to the start of the range and the
/// actual size in bytes is returned.
///
/// If there is no memory remaining (or another error occurs), the pointer returned will be null and the length will be zero.
#[inline]
pub fn heap_allocate(size_in_bytes: usize) -> (*mut (), usize) {
    unsafe {
        let mut p: *mut () = null_mut();
        let mut s: usize = 0;
        asm!(
            "mov x0, {is}",
            "mov x1, {p}",
            "mov x2, {s}",
            "svc #20",
            is = in(reg) size_in_bytes,
            p = in(reg) addr_of_mut!(p),
            s = in(reg) addr_of_mut!(s),
        );
        (p, s)
    }
}

/// Free some allocation created by [heap_allocate]. After this call, `ptr` is invalid.
#[inline]
pub fn heap_free(ptr: *mut (), size_in_bytes: usize) {
    unsafe {
        asm!(
            "mov x0, {p}",
            "mov x1, {s}",
            "svc #21",
            p = in(reg) ptr,
            s = in(reg) size_in_bytes,
        );
    }
}

/// Yield execution of the current thread, causing a new thread to be scheduled.
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
