use super::*;
use crate::memory::PhysicalAddress;
use alloc::boxed::Box;
use async_trait::async_trait;
use derive_more::Display;
use snafu::Snafu;

/// A BlockCache provides a cache layer on top of a BlockStore, allowing for unaligned reads/writes and minimizing unneccessary underlying operations
pub struct BlockCache {
    store: Box<dyn BlockStore>,
}

/// A BlockCacheRef is a immutable borrow on a slice in the cache.
pub struct BlockCacheRef<'s> {
    cache: &'s BlockCache,
    address: LogicalAddress,
    bytes: &'s [u8],
}

impl BlockCache {
    /// Slice into a block in the cache. If the block is not in the cache, it will be loaded.
    /// Size must be less than the size of a block
    async fn bytes<'s>(
        &'s self,
        address: LogicalAddress,
        size_in_bytes: usize,
    ) -> Option<BlockCacheRef<'s>> {
        todo!()
    }

    /// Copy bytes from the cache into a slice. Any unloaded blocks will be loaded, and copies can span multiple blocks
    async fn copy_bytes(&self, address: LogicalAddress, dest: &mut [u8]) {
        todo!()
    }

    /// Write bytes from a slice into the cache. Any unloaded blocks will be loaded, and writes can span multiple blocks. The cache will write the blocks back to the underlyinh storage when they are ejected from the cache.
    async fn write_bytes(&mut self, address: LogicalAddress, src: &[u8]) {
        todo!()
    }

    /// Update bytes in the cache in a certain range. All blocks in the range will be loaded into the cache if they are unloaded.
    async fn update_bytes(
        &mut self,
        address: LogicalAddress,
        size_in_bytes: usize,
        f: impl FnOnce(&mut [u8]),
    ) {
        todo!()
    }
}
