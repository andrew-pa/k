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
    pub kind: CommandKind,
    pub id: u16,
    pub completion_semaphore: u32,
    pub args: [u64; 4],
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
    pub kind: CompletionKind,
    pub respose_to_id: u16,
    pub result0: u32,
    pub result1: u64,
}
