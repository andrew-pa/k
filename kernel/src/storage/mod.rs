use crate::memory::PhysicalAddress;
use alloc::boxed::Box;
use async_trait::async_trait;
use derive_more::Display;
use snafu::Snafu;

#[derive(Copy, Clone, Display, PartialEq, Eq, PartialOrd, Ord)]
#[display(fmt = "L:0x{:x}", _0)]
pub struct LogicalAddress(pub u64);

#[derive(Debug, Snafu)]
pub enum Error {
    Memory { source: crate::memory::MemoryError },
    DeviceError,
}

#[async_trait]
pub trait BlockStore {
    // TODO: support non-contiguous source/destinations in physical memory
    // TODO: report how big the store is

    /// Block size supported by this store, in bytes
    fn supported_block_size(&self) -> usize;

    /// Read num_blocks from the store at source_addr.
    /// Destination must be a buffer of size supported_block_size() * num_blocks.
    /// Returns the number of blocks read, or an error if one occurred
    async fn read_blocks(
        &mut self,
        source_addr: LogicalAddress,
        destination_addr: PhysicalAddress,
        num_blocks: usize,
    ) -> Result<usize, Error>;

    /// Write the data at source into the blocks starting at dest_addr.
    /// The size of source must be a multiple of supported_block_size()
    /// Returns the number of blocks written, or an error if one occurred
    async fn write_blocks(
        &mut self,
        source_addr: PhysicalAddress,
        destination_addr: LogicalAddress,
        num_blocks: usize,
    ) -> Result<usize, Error>;
}

pub mod block_cache;
pub mod nvme;
