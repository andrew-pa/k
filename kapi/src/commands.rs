//! Asynchronous commands that can be submitted to the kernel from user-space via a [crate::queue::Queue].

use bytemuck::Zeroable;

use crate::queue::QueueId;

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
}

unsafe impl Zeroable for Kind {}

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
#[derive(Debug, Clone, Zeroable)]
pub struct Command {
    /// The type of the command.
    pub kind: Kind,
    /// The ID number for this command that will be repeated in completions that are associated with it.
    pub id: u16,
}
