//! API definitions and helpers for interacting with the kernel from user space.
#![no_std]
#![feature(non_null_convenience)]

use bitfield::bitfield;
use bytemuck::Contiguous;

pub mod queue;
pub mod system_calls;

#[repr(u16)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum CommandKind {
    Invalid = 0,
    Test,
    SpawnProcess,
    Reserved(u16),
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct Command {
    pub kind: CommandKind,
    pub id: u16,
    pub args: [u64; 4],
}

#[repr(u16)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum SuccessCode {
    Success = 0,
}

unsafe impl Contiguous for SuccessCode {
    type Int = u16;

    const MAX_VALUE: Self::Int = 0;

    const MIN_VALUE: Self::Int = 0;
}

#[repr(u16)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidStatusCode = 0,
    UnknownCommand,
}

unsafe impl Contiguous for ErrorCode {
    type Int = u16;

    const MAX_VALUE: Self::Int = 1;

    const MIN_VALUE: Self::Int = 0;
}

bitfield! {
    #[derive(Default, Clone, Copy, Eq, PartialEq)]
    pub struct CompletionStatus(u16);
    impl Debug;
    bool, is_error, set_error: 15;
    u16, code, set_code: 14, 0;
}

impl CompletionStatus {
    pub fn error_code(&self) -> Option<ErrorCode> {
        self.is_error()
            .then(|| self.code())
            .and_then(ErrorCode::from_integer)
    }

    pub fn success_code(&self) -> Option<SuccessCode> {
        (!self.is_error())
            .then(|| self.code())
            .and_then(SuccessCode::from_integer)
    }

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

impl From<SuccessCode> for CompletionStatus {
    fn from(value: SuccessCode) -> Self {
        let mut s = CompletionStatus::default();
        s.set_error(false);
        s.set_code(value.into_integer());
        s
    }
}

impl From<ErrorCode> for CompletionStatus {
    fn from(value: ErrorCode) -> Self {
        let mut s = CompletionStatus::default();
        s.set_error(true);
        s.set_code(value.into_integer());
        s
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct Completion {
    pub status: CompletionStatus,
    pub response_to_id: u16,
    pub result0: u32,
    pub result1: u64,
}
