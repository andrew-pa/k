//! Asynchronous commands that can be submitted to the kernel from user-space via a [crate::queue::Queue].

use bytemuck::Zeroable;

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
/// Completion type: [crate::completions::Test].
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Test {
    /// Value to return back.
    pub arg: u64,
}
impl_into_kind!(Test);

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
    Invalid = 0,
    Test(Test),

    CreateQueue,
    DestroyQueue,
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
    /// The ID number for this command that will be repeated in completions that are associated with it.
    pub id: u16,
    /// The type of the command.
    pub kind: Kind,
}
