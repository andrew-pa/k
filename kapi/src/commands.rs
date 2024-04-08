//! Asynchronous commands that can be submitted to the kernel from user-space via a [crate::queue::Queue].
use crate::{queue::QueueId, ProcessId, ThreadId};

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
///
/// Any pending messages will be ignored. If there were in-flight messages, they may still complete
/// but (TODO: where should they go?).
/// Submission queues must be destroyed before their associated completion queue.
/// [Completion Type][crate::completions::Kind::Success].
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

/// A reference to a path in the registry which names a resource.
///
/// # Safety
/// It is up to the user to ensure that the `text` pointer is valid until the path is no longer in
/// use (i.e. the completion to the command containing the path has been received).
/// It is additionally up to the user to make sure that the provided pointer is in fact [Send].
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Path {
    /// UTF-8 encoded text of the path.
    pub text: *const u8,
    /// The number of bytes that make up the path.
    pub len: usize,
}
unsafe impl Send for Path {}

/// Spawn a new process.
/// [Completion Type][crate::completions::NewProcess]
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnProcess {
    /// Path to executable binary on the file system, in ELF format.
    pub binary_path: Path,
    /// If true, an additional completion will be sent when the main thread of this process exits.
    /// [Completion Type][crate::completions::ThreadExit]
    pub send_completion_on_main_thread_exit: bool,
}
impl_into_kind!(SpawnProcess);

/// Kill a process. This will cause all threads in the process to exit immediately.
/// [Completion Type][crate::completions::Success]
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillProcess {
    /// The ID of the process to kill.
    pub process_id: ProcessId,
}
impl_into_kind!(KillProcess);

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
    KillProcess(KillProcess),
}

impl Kind {
    /// Get the enum discriminant value that identifies this command.
    pub fn discriminant(&self) -> u16 {
        // SAFETY: Because `Self` is marked `repr(u16)`, its layout is a `repr(C)` `union`
        // between `repr(C)` structs, each of which has the `u16` discriminant as its first
        // field, so we can read the discriminant without offsetting the pointer.
        unsafe { *<*const _>::from(self).cast::<u16>() }
    }
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
