//! Asynchronous commands that can be submitted to the kernel from user-space via a [crate::queue::Queue].

use bytemuck::{Contiguous, Pod, Zeroable};

use crate::queue::QueueId;

/// Type of command.
///
/// Each command is further documented by the constructor in [Command].
#[repr(u16)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Contiguous)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum Kind {
    Invalid = 0,
    Test,
    SpawnProcess,

    CreateQueue,
    DestroyQueue
}

unsafe impl Zeroable for Kind {}
unsafe impl Pod for Kind {}

/// A command that can be sent to the kernel.
#[repr(C)]
#[derive(Debug, Copy, Clone, Zeroable, Pod)]
pub struct Command {
    /// The type of the command.
    pub kind: Kind,
    /// The ID number for this command that will be repeated in completions that are associated with
    /// it.
    pub id: u16,
    ///
    pub reserved: u32,
    /// Argument values for the command, depending on the type.
    pub args: [u64; 4],
}

impl Command {
    /// Start a new command builder.
    pub fn builder() -> CommandBuilder {
        CommandBuilder {
            cmd: Command::zeroed()
        }
    }
}

/// Builder helper for creating commands.
pub struct CommandBuilder {
    cmd: Command
}

impl CommandBuilder {
    /// Finish building the command.
    #[inline]
    pub fn build(self) -> Command {
        self.cmd
    }

    /// Set the command ID.
    #[inline]
    pub fn id(mut self, id: u16) -> Self {
        self.cmd.id = id;
        self
    }

    /// Create a [Kind::Test] command.
    #[inline]
    pub fn test(mut self) -> Self {
        self.cmd.kind = Kind::Test;
        self.cmd.args[0] = 42;
        self
    }
}
