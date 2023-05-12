use crate::memory::PhysicalAddress;
use alloc::boxed::Box;
use async_trait::async_trait;

pub struct LogicalAddress(usize);

pub enum Error {}

#[async_trait]
pub trait BlockStore {
    /// Block size supported by this store, in bytes
    fn supported_block_size(&self) -> usize;

    /// Read num_blocks from the store at source_addr. Destination must be a buffer of size supported_block_size() * num_blocks
    async fn read_blocks(
        &mut self,
        source_addr: LogicalAddress,
        num_blocks: usize,
        destination_addr: PhysicalAddress,
    ) -> Result<usize, Error>;

    /// Write the data at source into the blocks starting at dest_addr. The size of source must be a multiple of supported_block_size()
    async fn write_blocks(
        &mut self,
        dest_addr: LogicalAddress,
        source_addr: PhysicalAddress,
        num_blocks: usize,
    ) -> Result<usize, Error>;
}
