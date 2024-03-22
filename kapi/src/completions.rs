//! Asynchronous results that can be received in user-space from the kernel via a [crate::queue::Queue].
use bitfield::bitfield;
use bytemuck::{Contiguous, Zeroable};

/// Codes representing various kinds of successful completions or events.
#[repr(u16)]
#[non_exhaustive]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Contiguous)]
pub enum SuccessCode {
    /// General success.
    Success = 0,
}

/// Codes representing various kinds of failures.
#[repr(u16)]
#[non_exhaustive]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Contiguous)]
pub enum ErrorCode {
    /// The status code found in the completion was invalid.
    InvalidStatusCode = 0,
    /// The command kind was unknown by the kernel.
    UnknownCommand,

    /// The operation requested is unsupported for a particular resource.
    UnsupportedOperation,

    /// The system has run out of memory.
    OutOfMemory,
    /// The resource referenced could not be found.
    NotFound,
    /// An argument was out of bounds.
    OutOfBounds,

    /// Data provided was in an incorrect format.
    BadFormat,

    /// The underlying device returned an error.
    Device,

    /// An internal kernel error occurred, check the system log for details.
    Internal,
}

bitfield! {
    /// The result status of a completion.
    #[derive(Default, Clone, Copy, Eq, PartialEq, Zeroable)]
    pub struct Status(u16);
    impl Debug;
    bool, is_error, set_error: 15;
    u16, code, set_code: 14, 0;
}

impl Status {
    /// If this completion represents an error, then this returns the code specifying what error occurred.
    pub fn error_code(&self) -> Option<ErrorCode> {
        self.is_error()
            .then(|| self.code())
            .and_then(ErrorCode::from_integer)
    }

    /// If this completion represents a success, then this returns the exact success code.
    pub fn success_code(&self) -> Option<SuccessCode> {
        (!self.is_error())
            .then(|| self.code())
            .and_then(SuccessCode::from_integer)
    }

    /// Convert the completion status into a regular [Result] containing the received status code.
    pub fn into_result(self) -> Result<SuccessCode, ErrorCode> {
        if self.is_error() {
            Err(ErrorCode::from_integer(self.code()).unwrap_or(ErrorCode::InvalidStatusCode))
        } else if let Some(s) = SuccessCode::from_integer(self.code()) {
            Ok(s)
        } else {
            Err(ErrorCode::InvalidStatusCode)
        }
    }
}

impl From<SuccessCode> for Status {
    fn from(value: SuccessCode) -> Self {
        let mut s = Status::default();
        s.set_error(false);
        s.set_code(value.into_integer());
        s
    }
}

impl From<ErrorCode> for Status {
    fn from(value: ErrorCode) -> Self {
        let mut s = Status::default();
        s.set_error(true);
        s.set_code(value.into_integer());
        s
    }
}

/// A completion/event that can be receved from the kernel.
#[repr(C)]
#[derive(Debug, Copy, Clone, Zeroable)]
pub struct Completion {
    /// The status of the command that caused this completion.
    pub status: Status,
    /// The ID number for the command that caused this completion.
    pub response_to_id: u16,
    /// First additional result value.
    pub result0: u32,
    /// Second additional result value.
    pub result1: u64,
}
