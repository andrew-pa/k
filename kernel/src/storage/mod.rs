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
    DeviceError,
}

// TODO: these definitions should probably just be in storage/mod.rs, maybe the block cache goes in
// a seperate file

#[async_trait]
pub trait BlockStore {
    // TODO: support non-contiguous source/destinations in physical memory

    /// Block size supported by this store, in bytes
    fn supported_block_size(&self) -> usize;

    /// Read num_blocks from the store at source_addr.
    /// Destination must be a buffer of size supported_block_size() * num_blocks.
    /// Returns the number of blocks read, or an error if one occurred
    async fn read_blocks(
        &mut self,
        source_addr: LogicalAddress,
        num_blocks: usize,
        destination_addr: PhysicalAddress,
    ) -> Result<usize, Error>;

    /// Write the data at source into the blocks starting at dest_addr.
    /// The size of source must be a multiple of supported_block_size()
    /// Returns the number of blocks written, or an error if one occurred
    async fn write_blocks(
        &mut self,
        destination_addr: LogicalAddress,
        source_addr: PhysicalAddress,
        num_blocks: usize,
    ) -> Result<usize, Error>;
}

pub mod block_cache;
pub mod nvme;
