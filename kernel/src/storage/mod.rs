//! Storage device drivers i.e. block devices.
use crate::{
    error::Error,
    memory::{MemoryError, PhysicalAddress},
};
use alloc::{boxed::Box, string::String};
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

// TODO: more specificity
#[derive(Debug, Snafu)]
pub enum StorageError {
    /// A source/destination vector was provided that is invalid
    BadVector {
        reason: &'static str,
        entry: Option<(PhysicalAddress, usize)>,
    },
    /// An error occurred on the device.
    DeviceError { reason: String },
    /// Failed to allocate temporary memory required by driver.
    MemoryError { source: MemoryError },
}

#[async_trait]
pub trait BlockStore: Send {
    // TODO: support non-contiguous source/destinations in physical memory
    // TODO: report how big the store is

    /// Block size supported by this store, in bytes
    fn supported_block_size(&self) -> usize;

    /// Read num_blocks from the store at source_addr, copying the data to the regions in destinations sequentially.
    /// Each destination is comprised of a (physical address, block count N) pair that indicates
    /// the place to write N blocks to before moving to the next destination.
    /// The destination vector must have a total size of num_blocks.
    /// Returns the number of blocks read, or an error if one occurred
    // TODO: probably shouldn't need &mut self?
    async fn read_blocks<'a>(
        &mut self,
        source_addr: BlockAddress,
        destinations: &'a [(PhysicalAddress, usize)],
    ) -> Result<usize, StorageError>;

    /// Write the data at each source region sequentially into the blocks starting at dest_addr.
    /// Each source is comprised of a (physical address, block count N) pair that indicates
    /// the place to read N blocks from before moving to the next source.
    /// The source vector must have a total size of num_blocks.
    /// Returns the number of blocks written, or an error if one occurred
    async fn write_blocks<'a>(
        &mut self,
        sources: &'a [(PhysicalAddress, usize)],
        destination_addr: BlockAddress,
    ) -> Result<usize, StorageError>;
}

#[async_trait]
impl BlockStore for Box<dyn BlockStore> {
    fn supported_block_size(&self) -> usize {
        self.as_ref().supported_block_size()
    }

    async fn read_blocks<'a>(
        &mut self,
        source_addr: BlockAddress,
        destination_addrs: &'a [(PhysicalAddress, usize)],
    ) -> Result<usize, StorageError> {
        self.as_mut()
            .read_blocks(source_addr, destination_addrs)
            .await
    }

    async fn write_blocks<'a>(
        &mut self,
        source_addrs: &'a [(PhysicalAddress, usize)],
        destination_addr: BlockAddress,
    ) -> Result<usize, StorageError> {
        self.as_mut()
            .write_blocks(source_addrs, destination_addr)
            .await
    }
}

pub mod block_cache;
pub mod nvme;
