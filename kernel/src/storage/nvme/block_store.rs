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
    fn supported_block_size(&self) -> usize {
        self.supported_block_size
    }

    async fn read_blocks(
        &mut self,
        source_addr: LogicalAddress,
        num_blocks: usize,
        destination_addr: PhysicalAddress,
    ) -> Result<usize, Error> {
        let cmp = self.io_cq.wait_for_completion(
            self.io_sq.begin()
                .expect("NVMe submission queue full. TODO: we should be able to await for a new spot in the queue rather than panic")
                .set_namespace_id(self.namespace_id)
                .set_data_ptr_single(destination_addr)
                .read(source_addr, num_blocks as u16)
        ).await;
        match (cmp.status.status_code_type(), cmp.status.status_code()) {
            (0, 0) => Ok(num_blocks),
            _ => {
                log::error!("failed to do NVMe read at {source_addr} of size {num_blocks} to {destination_addr}: {cmp:?}");
                // TODO: this could maybe be more specific
                Err(Error::DeviceError)
            }
        }
    }

    async fn write_blocks(
        &mut self,
        destination_addr: LogicalAddress,
        source_addr: PhysicalAddress,
        num_blocks: usize,
    ) -> Result<usize, Error> {
        let cmp = self.io_cq.wait_for_completion(
            self.io_sq.begin()
                .expect("NVMe submission queue full. TODO: we should be able to await for a new spot in the queue rather than panic")
                .set_namespace_id(self.namespace_id)
                .set_data_ptr_single(source_addr)
                .write(destination_addr, num_blocks as u16)
        ).await;
        match (cmp.status.status_code_type(), cmp.status.status_code()) {
            (0, 0) => Ok(num_blocks),
            _ => {
                log::error!("failed to do NVMe write from {source_addr} of size {num_blocks} to {destination_addr}: {cmp:?}");
                // TODO: this could maybe be more specific
                Err(Error::DeviceError)
            }
        }
    }
}
