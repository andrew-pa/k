#![no_std]

#[repr(u16)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum CommandKind {
    Invalid = 0,
    Test,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct Command {
    kind: CommandKind,
    id: u16,
    completion_semaphore: u32,
    args: [u64; 4],
}

#[repr(u16)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum CompletionKind {
    Invalid = 0,
    Success,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct Completion {
    kind: CompletionKind,
    respose_to_id: u16,
    result0: u32,
    result1: u64,
}
