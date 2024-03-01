//! File system drivers.
use alloc::boxed::Box;
use async_trait::async_trait;
use snafu::Snafu;

use crate::memory::PhysicalAddress;

/// File system related errors.
#[derive(Debug, Snafu)]
pub enum Error {
    Memory {
        source: crate::memory::MemoryError,
    },
    Storage {
        source: crate::storage::Error,
    },
    Registry {
        source: crate::registry::RegistryError,
    },
    BadMetadata {
        message: &'static str,
        value: usize,
    },
    OutOfBounds {
        value: usize,
        bound: usize,
        message: &'static str,
    },
    Other {
        reason: &'static str,
        source: Box<dyn snafu::Error + Send + Sync>,
    },
}

/// A File is a mappable persistent data blob
#[async_trait]
pub trait File {
    /// Returns the length of the store in bytes.
    fn len(&self) -> u64;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Read `num_pages` pages starting at `src_offset` (in bytes) from the file into `dest_address` in memory
    async fn load_pages(
        &mut self,
        src_offset: u64,
        dest_address: PhysicalAddress,
        num_pages: usize,
    ) -> Result<(), Error>;

    /// Write any changes from the in-memory file contents at `src_address` back to the persistent store
    async fn flush_pages(
        &mut self,
        dest_offset: u64,
        src_address: PhysicalAddress,
        num_pages: usize,
    ) -> Result<(), Error>;
}

pub mod fat;
