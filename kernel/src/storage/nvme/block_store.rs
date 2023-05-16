use crate::{
    io::{BlockStore, Error, LogicalAddress},
    memory::PhysicalAddress,
};
use alloc::boxed::Box;
use async_trait::async_trait;

use super::{interrupt::CompletionQueueHandle, queue::SubmissionQueue};

pub(super) struct NamespaceBlockStore {
    pub total_size: u64,
    pub capacity: u64,
    pub utilitization: u64,
    pub namespace_id: u32,
    pub supported_block_size: usize,
    pub io_sq: SubmissionQueue,
    pub io_cq: CompletionQueueHandle,
}

#[async_trait]
impl BlockStore for NamespaceBlockStore {
    /// Block size supported by this store, in bytes
    fn supported_block_size(&self) -> usize {
        self.supported_block_size
    }

    /// Read num_blocks from the store at source_addr. Destination must be a buffer of size supported_block_size() * num_blocks
    async fn read_blocks(
        &mut self,
        source_addr: LogicalAddress,
        num_blocks: usize,
        destination_addr: PhysicalAddress,
    ) -> Result<usize, Error> {
        todo!()
    }

    /// Write the data at source into the blocks starting at dest_addr. The size of source must be a multiple of supported_block_size()
    async fn write_blocks(
        &mut self,
        dest_addr: LogicalAddress,
        source_addr: PhysicalAddress,
        num_blocks: usize,
    ) -> Result<usize, Error> {
        todo!()
    }
}
