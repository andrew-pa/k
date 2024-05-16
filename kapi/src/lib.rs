//! API definitions and helpers for interacting with the kernel from user space.
#![no_std]
#![deny(missing_docs)]

use core::num::NonZeroU32;
use core::ptr::{null, null_mut};

/// The unique ID of a process.
pub type ProcessId = NonZeroU32;
/// The system-wide unique ID of a thread.
pub type ThreadId = u32;
/// The process-unique ID of an open file.
pub type FileHandle = NonZeroU32;

/// An unsigned integer suitably large for a file size or offset value.
pub type FileUSize = u64;
/// An signed integer suitably large for a file offset value.
pub type FileISize = i64;

pub mod commands;
pub mod completions;
pub mod queue;
pub mod system_calls;

/// The maximum number of bytes in a path.
pub const PATH_MAX_LEN: usize = 4096;

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

impl From<&str> for Path {
    fn from(value: &str) -> Self {
        assert!(value.len() <= PATH_MAX_LEN);
        Self {
            text: value.as_ptr(),
            len: value.len(),
        }
    }
}

/// A slice of memory containing arbitrary data.
///
/// # Safety
/// It is up to the user to ensure that the `data` pointer is valid until the buffer is no longer in
/// use (i.e. the completion to the command containing the path has been received).
/// It is additionally up to the user to make sure that the provided pointer is in fact [Send].
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Buffer {
    /// Pointer to the start of buffer.
    pub data: *const u8,
    /// Length of the buffer in bytes.
    pub len: usize,
}
unsafe impl Send for Buffer {}

impl Buffer {
    /// Create an empty buffer that contains no bytes.
    pub fn empty() -> Self {
        Self {
            data: null(),
            len: 0,
        }
    }
}

impl From<&[u8]> for Buffer {
    fn from(value: &[u8]) -> Self {
        Self {
            data: value.as_ptr(),
            len: value.len(),
        }
    }
}

/// A slice of memory containing arbitrary mutable data.
///
/// # Safety
/// It is up to the user to ensure that the `data` pointer is valid until the buffer is no longer in
/// use (i.e. the completion to the command containing the path has been received).
/// It is additionally up to the user to make sure that the provided pointer is in fact [Send], and
/// that the Rust aliasing/sharing rules are not violated.
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferMut {
    /// Pointer to the start of buffer.
    pub data: *mut u8,
    /// Length of the buffer in bytes.
    pub len: usize,
}
unsafe impl Send for BufferMut {}

impl BufferMut {
    /// Create an empty buffer that contains no bytes.
    pub fn empty() -> Self {
        Self {
            data: null_mut(),
            len: 0,
        }
    }
}

impl From<&mut [u8]> for BufferMut {
    fn from(value: &mut [u8]) -> Self {
        Self {
            data: value.as_mut_ptr(),
            len: value.len(),
        }
    }
}
