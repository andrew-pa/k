//! File system drivers.
use alloc::boxed::Box;
use async_trait::async_trait;
use kapi::FileUSize;
use snafu::Snafu;

use crate::error::Error;

/// File system related errors.
#[derive(Debug, Snafu)]
pub enum FsError {
    BadMetadata {
        message: &'static str,
        value: usize,
    },
    #[snafu(display("out of bounds operation {message}: value={value}, bound={bound}"))]
    OutOfBounds {
        value: FileUSize,
        bound: FileUSize,
        message: &'static str,
    },
}

/// A File is a persistent blob of bytes.
#[async_trait]
pub trait File: Send {
    /// Returns the length of the store in bytes.
    fn len(&self) -> FileUSize;

    /// Returns true if the file is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Read bytes starting from `src_offset` in the file into the buffers in `destinations`.
    /// The total length of `destinations` must fit within the file or the read will fail with an OutOfBounds error.
    /// No IO will occur in this case.
    /// Implementations will not modify the `destinations` slice directly, it is mutable only to
    /// allow mutation of the inner slices i.e. to mark that the borrow is exclusive.
    async fn read<'v, 'dest>(
        &self,
        src_offset: FileUSize,
        destinations: &'v mut [&'dest mut [u8]],
    ) -> Result<(), Error>;

    /// Write bytes starting at `dst_offset` in the file from the buffers in `sources`.
    /// The total length of `sources` must fit within the file or the write will fail with an OutOfBounds error.
    /// No IO will occur in this case.
    async fn write<'v, 'src>(
        &mut self,
        dst_offset: FileUSize,
        sources: &'v [&'src [u8]],
    ) -> Result<(), Error>;
}

pub mod fat;
