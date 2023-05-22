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

/// A BlockCache provides a cache layer on top of a BlockStore, allowing for unaligned reads/writes and minimizing unneccessary underlying operations
pub struct BlockCache {
    store: Box<dyn BlockStore>,
}

/// A BlockCacheRef is a immutable borrow on a slice in the cache.
pub struct BlockCacheRef<'s> {
    cache: &'s BlockCache,
    address: LogicalAddress,
    bytes: &'s [u8]
}

impl BlockCache {
    /// Slice into a block in the cache. If the block is not in the cache, it will be loaded.
    /// Size must be less than the size of a block
    async fn bytes<'s>(&'s self, address: LogicalAddress, size_in_bytes: usize) -> Option<BlockCacheRef<'s>> {todo!()}

    /// Copy bytes from the cache into a slice. Any unloaded blocks will be loaded, and copies can span multiple blocks
    async fn copy_bytes(&self, address: LogicalAddress, dest: &mut [u8]) {todo!()}

    /// Write bytes from a slice into the cache. Any unloaded blocks will be loaded, and writes can span multiple blocks. The cache will write the blocks back to the underlyinh storage when they are ejected from the cache.
    async fn write_bytes(&mut self, address: LogicalAddress, src: &[u8]) {todo!()}

    /// Update bytes in the cache in a certain range. All blocks in the range will be loaded into the cache if they are unloaded.
    async fn update_bytes(&mut self, address: LogicalAddress, size_in_bytes: usize, f: impl FnOnce(&mut [u8])) { todo!() }
}
