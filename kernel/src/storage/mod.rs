use crate::memory::PhysicalAddress;
use alloc::boxed::Box;
use async_trait::async_trait;
use derive_more::Display;
use snafu::Snafu;

/// The address of a block in a [BlockStore]
#[derive(Copy, Clone, Display, PartialEq, Eq, PartialOrd, Ord)]
#[display(fmt = "B:0x{:x}", _0)]
pub struct BlockAddress(pub u64);

impl core::fmt::Debug for BlockAddress {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "B:0x{:x}", self.0)
    }
}

#[derive(Debug, Snafu)]
pub enum Error {
    Memory { source: crate::memory::MemoryError },
    DeviceError,
}

#[async_trait]
pub trait BlockStore: Send {
    // TODO: support non-contiguous source/destinations in physical memory
    // TODO: report how big the store is

    /// Block size supported by this store, in bytes
    fn supported_block_size(&self) -> usize;

    /// Read num_blocks from the store at source_addr.
    /// Destination must be a buffer of size supported_block_size() * num_blocks.
    /// Returns the number of blocks read, or an error if one occurred
    async fn read_blocks(
        &mut self,
        source_addr: BlockAddress,
        destination_addr: PhysicalAddress,
        num_blocks: usize,
    ) -> Result<usize, Error>;

    /// Write the data at source into the blocks starting at dest_addr.
    /// The size of source must be a multiple of supported_block_size()
    /// Returns the number of blocks written, or an error if one occurred
    async fn write_blocks(
        &mut self,
        source_addr: PhysicalAddress,
        destination_addr: BlockAddress,
        num_blocks: usize,
    ) -> Result<usize, Error>;
}

#[async_trait]
impl BlockStore for Box<dyn BlockStore> {
    fn supported_block_size(&self) -> usize {
        self.as_ref().supported_block_size()
    }

    async fn read_blocks(
        &mut self,
        source_addr: BlockAddress,
        destination_addr: PhysicalAddress,
        num_blocks: usize,
    ) -> Result<usize, Error> {
        self.as_mut()
            .read_blocks(source_addr, destination_addr, num_blocks)
            .await
    }

    async fn write_blocks(
        &mut self,
        source_addr: PhysicalAddress,
        destination_addr: BlockAddress,
        num_blocks: usize,
    ) -> Result<usize, Error> {
        self.as_mut()
            .write_blocks(source_addr, destination_addr, num_blocks)
            .await
    }
}

pub mod block_cache;
pub mod nvme;
