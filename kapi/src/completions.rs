//! Asynchronous results that can be received in user-space from the kernel via a [crate::queue::Queue].
use bytemuck::{Contiguous, Zeroable};

use crate::{queue::QueueId, ProcessId};

macro_rules! impl_into_kind {
    ($t:ident) => {
        impl From<$t> for Kind {
            fn from(value: $t) -> Self {
                Kind::$t(value)
            }
        }
    };
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
impl From<ErrorCode> for Kind {
    fn from(value: ErrorCode) -> Self {
        Kind::Err(value)
    }
}

/// Response to a [crate::commands::Test] command.
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Test {
    /// Current process ID.
    pub pid: ProcessId,
    /// Value of `arg`.
    pub arg: u64,
}
impl_into_kind!(Test);

/// Response to a [crate::commands::CreateSubmissionQueue] or [crate::commands::CreateCompletionQueue] command.
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewQueue {
    /// The ID of the new queue.
    pub id: QueueId,
    /// The address of the start of the queue.
    pub start: usize,
    /// The size of the queue in bytes.
    pub size_in_bytes: usize,
}
impl_into_kind!(NewQueue);

/// Type of completion and any resulting values returned by the command.
#[repr(u16)]
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum Kind {
    /// The completion is actually invalid.
    Invalid = 0,
    /// General success result for commands that don't return any values.
    Success = 1,
    Test(Test),
    NewQueue(NewQueue),
    /// The command failed to complete successfully.
    Err(ErrorCode),
}

unsafe impl Zeroable for Kind {}

/// A completion/event that can be receved from the kernel.
#[repr(C)]
#[derive(Debug, Clone, Zeroable)]
pub struct Completion {
    /// The ID number for the command that caused this completion.
    pub response_to_id: u16,
    /// The type of the completion.
    pub kind: Kind,
}
