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

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Read `num_pages` pages starting at `src_offset` (in bytes) from the file into `dest_address` in memory
    async fn load_pages(
        &mut self,
        src_offset: FileUSize,
        dest_address: PhysicalAddress,
        num_pages: usize,
    ) -> Result<(), Error>;

    /// Write any changes from the in-memory file contents at `src_address` back to the persistent store
    async fn flush_pages(
        &mut self,
        dest_offset: FileUSize,
        src_address: PhysicalAddress,
        num_pages: usize,
    ) -> Result<(), Error>;
}

pub mod fat;
