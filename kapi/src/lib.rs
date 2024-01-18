#![no_std]

use core::num::NonZeroU32;

#[repr(u16)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum CommandKind {
    Invalid = 0,
    Test,
    Reserved(u16),
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct Command {
    pub kind: CommandKind,
    pub id: u16,
    pub completion_semaphore: Option<NonZeroU32>,
    pub args: [u64; 4],
}

#[repr(u16)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum CompletionKind {
    Invalid = 0,
    Success,
    UnknownCommand,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct Completion {
    pub kind: CompletionKind,
    pub response_to_id: u16,
    pub result0: u32,
    pub result1: u64,
}
