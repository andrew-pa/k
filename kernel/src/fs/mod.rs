//! File system drivers.
use alloc::boxed::Box;
use async_trait::async_trait;
use kapi::FileUSize;
use snafu::Snafu;

use crate::{error::Error, memory::PhysicalAddress};

/// File system related errors.
#[derive(Debug, Snafu)]
pub enum FsError {
    BadMetadata {
        message: &'static str,
        value: usize,
    },
    OutOfBounds {
        value: usize,
        bound: usize,
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
    async fn read<'v, 'dest>(
        &self,
        src_offset: FileUSize,
        destinations: &'v [&'dest mut [u8]],
    ) -> Result<(), Error>;

    /// Write bytes starting at `dst_offset` in the file from the buffers in `sources`.
    /// The total length of `sources` must fit within the file or the write will fail with an OutOfBounds error.
    /// No IO will occur in this case.
    async fn write<'v, 'src>(
        &mut self,
        dst_offset: FileUSize,
        sources: &'v [&'src [u8]],
    ) -> Result<(), Error>;

    /// Read `num_pages` pages starting at `src_offset` (in bytes) from the file into `dest_address` in memory
    async fn load_pages(
        &mut self,
        src_offset: FileUSize,
        dest_address: PhysicalAddress,
        num_pages: usize,
    ) -> Result<(), Error>;
}

pub mod fat;
