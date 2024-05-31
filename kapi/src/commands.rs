//! Asynchronous commands that can be submitted to the kernel from user-space via a [crate::queue::Queue].

use crate::{queue::QueueId, Buffer, BufferMut, FileHandle, FileUSize, Path, ProcessId, ThreadId};

macro_rules! impl_into_kind {
    ($t:ident) => {
        impl From<$t> for Kind {
            fn from(value: $t) -> Self {
                Kind::$t(value)
            }
        }
    };
}

/// Send a test command. The `arg` value will be echoed back, along with the current pid.
/// [Completion Type][crate::completions::Test].
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Test {
    /// Value to return back.
    pub arg: u64,
}
impl_into_kind!(Test);

/// Create a new submission queue for the process, which will be associated with a completion queue.
/// The completion queue must exist before the submission queue is created.
/// [Completion Type][crate::completions::NewQueue].
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateSubmissionQueue {
    /// Number of commands this queue will be able to hold.
    /// This may be rounded up to the next page, but must be at least 1.
    pub size: usize,
    /// ID of the completion queue that completions for commands from this queue will be sent to.
    pub associated_completion_queue: QueueId,
}
impl_into_kind!(CreateSubmissionQueue);

/// Create a new completion queue for the process.
/// [Completion Type][crate::completions::NewQueue].
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateCompletionQueue {
    /// Number of completions this queue will be able to hold.
    /// This may be rounded up to the next page, but must be at least 1.
    pub size: usize,
}
impl_into_kind!(CreateCompletionQueue);

/// Destroy a queue.
/// [Completion Type][crate::completions::Kind::Success].
///
/// Any pending messages will be ignored. If there were in-flight messages, they may still complete
/// but (TODO: where should they go?).
/// Submission queues must be destroyed before their associated completion queue.
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestroyQueue {
    /// ID of the queue to destroy.
    pub id: QueueId,
}
impl_into_kind!(DestroyQueue);

/// Spawn a new thread in the current process.
/// [Completion Type][crate::completions::NewThread]
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnThread {
    /// The function that will serve as the thread's entry point.
    pub entry_point: fn(*mut ()) -> !,
    /// Initial size of the stack allocated for this thread in bytes.
    /// Rounded up to the nearest page.
    pub stack_size: usize,
    /// A pointer to any user data that will be passed to the thread verbatim.
    ///
    /// It is up to the sender to ensure that this data is correctly synchronized
    /// (i.e. points to something that is Send).
    pub user_data: *mut (),
    /// If true, an additional completion will be sent when the thread exits.
    /// [Completion Type][crate::completions::ThreadExit]
    pub send_completion_on_exit: bool,
}
/// The `user_data` pointer should be find to send, since that is what is expected.
unsafe impl Send for SpawnThread {}
impl_into_kind!(SpawnThread);

/// Request a completion to be sent when a thread exits.
/// This command will only complete when the thread finishes.
/// If the thread has already exited when this command is processed, then TODO
/// [Completion Type][crate::completions::ThreadExit]
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchThread {
    /// The ID of the thread to watch.
    pub thread_id: ThreadId,
}
impl_into_kind!(WatchThread);

/// Spawn a new process.
/// [Completion Type][crate::completions::NewProcess]
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnProcess {
    /// Path to executable binary on the file system, in ELF format.
    pub binary_path: Path,
    /// Additional parameters to provide to the process.
    /// The buffer will be copied into the process' address space.
    pub parameters: Buffer,
    /// If true, an additional completion will be sent when the main thread of this process exits.
    /// [Completion Type][crate::completions::ThreadExit]
    pub send_completion_on_main_thread_exit: bool,
}
impl_into_kind!(SpawnProcess);

/// Request a completion to be sent when a process exits.
/// This command will only complete when the last thread in the process exits, and the completion
/// will contain the exit code for that last thread.
/// If the process has already exited when this command is processed, then TODO
/// [Completion Type][crate::completions::ThreadExit]
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchProcess {
    /// The ID of the process to watch.
    pub process_id: ProcessId,
}
impl_into_kind!(WatchProcess);

/// Kill a process. This will cause all threads in the process to exit immediately.
/// [Completion Type][crate::completions::Success]
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillProcess {
    /// The ID of the process to kill.
    pub process_id: ProcessId,
}
impl_into_kind!(KillProcess);

/// Create a new file in some file system.
/// [Completion Type][crate::completions::OpenedFileHandle]
///
/// If the file already exists, it may have a different size than `initial_size`.
/// The correct, current file size will always be returned in the completion.
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateFile {
    /// The path that the file will be created under.
    pub path: Path,
    /// The initial size of the file in bytes.
    pub initial_size: usize,
    /// If true, the file will be opened if it already exists. If false, an error will be returned if the file exists.
    pub open_if_exists: bool,
}
impl_into_kind!(CreateFile);

/// Open a file that already exists on some file system.
/// [Completion Type][crate::completions::OpenedFileHandle]
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenFile {
    /// The path to the file to open.
    pub path: Path,
}
impl_into_kind!(OpenFile);

/// Read data from a file into a buffer.
/// [Completion Type][crate::completions::Success]
///
/// At most the entire buffer is filled with data from the file, starting from the offset.
/// If the length to read extends past the end of the file, an [crate::completions::ErrorCode::OutOfBounds] error will occur.
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadFile {
    /// The file to read from.
    pub src_handle: FileHandle,
    /// An offset in bytes from the start of the file to start reading from.
    pub src_offset: FileUSize,
    /// The destination buffer to fill. This buffer *must* stay valid until the completion is
    /// received for the read to be safe.
    pub dst_buffer: BufferMut,
}
impl_into_kind!(ReadFile);

/// Write data from a buffer into a file.
/// [Completion Type][crate::completions::Success]
///
/// At most the entire buffer is written to the file, starting from the offset.
/// If the length to write extends past the end of the file, an [crate::completions::ErrorCode::OutOfBounds] error will occur.
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteFile {
    /// The file to write to.
    pub dst_handle: FileHandle,
    /// An offset in bytes from the start of the file to start writing to.
    pub dst_offset: FileUSize,
    /// The source buffer to read from. This buffer *must* stay valid until the completion is
    /// received for the write to be safe.
    pub src_buffer: Buffer,
}
impl_into_kind!(WriteFile);

/// Resize a file.
/// [Completion Type][crate::completions::Success]
///
/// If the new size is smaller than the current size, the file will be truncated and
/// the data discarded. If the new size is larger than the current, the file will have that many
/// zeros appended to the end.
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResizeFile {
    /// The file to resize.
    pub handle: FileHandle,
    /// The new size of the file in bytes.
    pub new_size: FileUSize,
}
impl_into_kind!(ResizeFile);

/// Close a file handle.
/// [Completion Type][crate::completions::Success]
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseFile {
    /// The file handle to close.
    pub handle: FileHandle,
}
impl_into_kind!(CloseFile);

/// Delete a file.
/// [Completion Type][crate::completions::Success]
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteFile {
    /// Path to the file to delete.
    pub path: Path,
}
impl_into_kind!(DeleteFile);

/// Type of command and any required parameters.
///
/// Each command is further documented on the inner parameter struct.
///
/// See [this RFC](https://github.com/rust-lang/rfcs/blob/master/text/2195-really-tagged-unions.md) for information about how this is layed out in memory.
#[repr(u16)]
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum Kind {
    /// The command is actually invalid.
    Invalid = 0,
    Test(Test),

    CreateCompletionQueue(CreateCompletionQueue),
    CreateSubmissionQueue(CreateSubmissionQueue),
    DestroyQueue(DestroyQueue),

    SpawnThread(SpawnThread),
    WatchThread(WatchThread),

    SpawnProcess(SpawnProcess),
    WatchProcess(WatchProcess),
    KillProcess(KillProcess),

    CreateFile(CreateFile),
    OpenFile(OpenFile),
    ReadFile(ReadFile),
    WriteFile(WriteFile),
    ResizeFile(ResizeFile),
    CloseFile(CloseFile),
    DeleteFile(DeleteFile),
}

/// A command that can be sent to the kernel.
#[repr(C)]
#[derive(Debug, Clone)]
pub struct Command {
    /// The type of the command.
    pub kind: Kind,
    /// The ID number for this command that will be repeated in completions that are associated with it.
    // TODO: this should probably be a u32
    pub id: u16,
}
