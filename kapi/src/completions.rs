//! Asynchronous results that can be received in user-space from the kernel via a [crate::queue::Queue].
use bytemuck::Contiguous;

use crate::{queue::QueueId, FileHandle, ProcessId, ThreadId};

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
    /// The command kind was unknown by the kernel.
    UnknownCommand,

    /// The operation requested is unsupported for a particular resource.
    UnsupportedOperation,

    /// The system has run out of memory.
    OutOfMemory,
    /// An argument was out of bounds.
    OutOfBounds,
    /// The kernel has run out of free IDs for a resource.
    OutOfIds,

    /// Data provided was in an incorrect format.
    BadFormat,

    /// The underlying device returned an error.
    Device,

    /// An ID (or handle) was provided that was not known to the kernel.
    InvalidId,
    /// A size was provided that is invalid for the operation.
    InvalidSize,
    /// The provided user space pointer was invalid.
    InvalidPointer,
    /// A string was provided that was not valid UTF-8.
    InvalidUtf8,

    /// The resource referenced could not be found.
    NotFound,
    /// The requsted creation cannot occur because the object already exists.
    AlreadyExists,
    /// The object was still in use when its destruction was requested.
    InUse,

    /// An internal kernel error occurred, check the system log for details.
    Internal,
}

impl From<ErrorCode> for Kind {
    fn from(value: ErrorCode) -> Self {
        Kind::Err(value)
    }
}

/// Response to a command with no returned values, but indicates success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Success;
impl From<Success> for Kind {
    fn from(_value: Success) -> Self {
        Kind::Success
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
}
impl_into_kind!(NewQueue);

/// Response to a [crate::commands::SpawnThread] command.
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewThread {
    /// The ID of the new thread.
    pub id: ThreadId,
}
impl_into_kind!(NewThread);

/// Response to a [crate::commands::WatchThread] command, or a [crate::commands::SpawnThread] with
/// `send_completion_on_exit` set to true.
#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadExit {
    /// The thread exited normally (i.e. by calling [exit][crate::system_calls::exit]).
    Normal(u16),
    /// The thread tried to access an address that was unmapped in the address space.
    PageFault,
    /// The thread was forcibly caused to exit by another process.
    Killed,
}
impl_into_kind!(ThreadExit);

/// Response to a [crate::commands::SpawnProcess] command.
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProcess {
    /// The ID of the new process.
    pub id: ProcessId,
}
impl_into_kind!(NewProcess);

/// Response to a [crate::commands::CreateFile] command.
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedFileHandle {
    /// The newly opened handle.
    pub handle: FileHandle,
    /// The size of the file in bytes.
    pub size: usize,
}
impl_into_kind!(OpenedFileHandle);

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
    /// The command failed to complete successfully.
    Err(ErrorCode),
    Test(Test),
    NewQueue(NewQueue),
    NewThread(NewThread),
    ThreadExit(ThreadExit),
    NewProcess(NewProcess),
    OpenedFileHandle(OpenedFileHandle),
}

/// A completion/event that can be receved from the kernel.
#[repr(C)]
#[derive(Debug, Clone)]
pub struct Completion {
    /// The ID number for the command that caused this completion.
    pub response_to_id: u16,
    /// The type of the completion.
    pub kind: Kind,
}
