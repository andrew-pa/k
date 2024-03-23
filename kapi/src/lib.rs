//! API definitions and helpers for interacting with the kernel from user space.
#![no_std]
#![feature(non_null_convenience)]
#![deny(missing_docs)]

use core::num::NonZeroU32;

/// The unique ID of a process.
pub type ProcessId = NonZeroU32;
/// The system-wide unique ID of a thread.
pub type ThreadId = u32;

pub mod commands;
pub mod completions;
pub mod queue;
pub mod system_calls;
