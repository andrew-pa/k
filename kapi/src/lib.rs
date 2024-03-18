#![no_std]
#![feature(non_null_convenience)]

use core::num::NonZeroU32;

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
    pub completion_semaphore: Option<NonZeroU32>,
    pub args: [u64; 4],
}

#[repr(u16)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum CompletionKind {
    Invalid = 0,
    Success,
    UnknownCommand,
    Error,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct Completion {
    pub kind: CompletionKind,
    pub response_to_id: u16,
    pub result0: u32,
    pub result1: u64,
}
