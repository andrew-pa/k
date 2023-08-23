use alloc::boxed::Box;
use async_trait::async_trait;
use snafu::Snafu;

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
}

/// Enumeration of possible methods to seek within an I/O object.
pub enum SeekFrom {
    /// Sets the offset to the provided number of bytes.
    Start(u64),
    /// Sets the offset to the size of this object plus the specified number of bytes.
    End(i64),
    /// Sets the offset to the current position plus the specified number of bytes.
    Current(i64),
}

/// A ByteStore is a byte-addressable store with an implicit read/write address.
#[async_trait]
pub trait ByteStore {
    /// Seek to an offset, in bytes, in a stream.
    ///
    /// If the seek operation completed successfully, this method returns the new position from the start of the stream. That position can be used later with SeekFrom::Start.
    async fn seek(&mut self, pos: SeekFrom) -> Result<u64, Error>;
    /// Pull some bytes from this source into the specified buffer, returning how many bytes were read.
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error>;
    /// Write a buffer into this writer, returning how many bytes were written.
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Error>;
}

pub mod fat;
