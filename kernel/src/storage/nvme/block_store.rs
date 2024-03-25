use crate::{
    memory::{PhysicalAddress, PAGE_SIZE},
    storage::{BlockAddress, BlockStore, StorageError},
};
use alloc::{boxed::Box, format};
use async_trait::async_trait;

use super::{interrupt::CompletionQueueHandle, queue::SubmissionQueue};

#[allow(unused)]
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

    async fn read_blocks<'a>(
        &mut self,
        source_addr: BlockAddress,
        destination_addrs: &'a [(PhysicalAddress, usize)],
    ) -> Result<usize, StorageError> {
        // log::trace!("read blocks {source_addr}[..{num_blocks}] -> {destination_addrs:?}");
        let total_num_blocks: u16 = destination_addrs
            .iter()
            .map(|(_, n)| n)
            .sum::<usize>()
            .try_into()
            .map_err(|_| {
                crate::storage::BadVectorSnafu {
                    reason: "cannot read more than 2^16 total blocks from NVMe device",
                    entry: None,
                }
                .build()
            })?;
        let cmp = self.io_cq.wait_for_completion(
            self.io_sq.begin()
                .expect("NVMe submission queue full. TODO: we should be able to await for a new spot in the queue rather than panic")
                .set_namespace_id(self.namespace_id)
                .set_data_ptrs(destination_addrs, PAGE_SIZE / self.supported_block_size)?
                .read(source_addr, total_num_blocks)
        ).await;
        match (cmp.status.status_code_type(), cmp.status.status_code()) {
            (0, 0) => Ok(total_num_blocks as usize),
            _ => {
                Err(StorageError::DeviceError {
                    reason: format!("failed to do NVMe read at {source_addr} of size {total_num_blocks} to {destination_addrs:?}: {cmp:?}")
                })
            }
        }
    }

    async fn write_blocks<'a>(
        &mut self,
        source_addrs: &'a [(PhysicalAddress, usize)],
        destination_addr: BlockAddress,
    ) -> Result<usize, StorageError> {
        // log::trace!("write blocks {destination_addr} <- {source_addrs:?}[..{num_blocks}]");
        let total_num_blocks: u16 = source_addrs
            .iter()
            .map(|(_, n)| n)
            .sum::<usize>()
            .try_into()
            .map_err(|_| {
                crate::storage::BadVectorSnafu {
                    reason: "cannot read more than 2^16 total blocks from NVMe device",
                    entry: None,
                }
                .build()
            })?;
        let cmp = self.io_cq.wait_for_completion(
            self.io_sq.begin()
                .expect("NVMe submission queue full. TODO: we should be able to await for a new spot in the queue rather than panic")
                .set_namespace_id(self.namespace_id)
                .set_data_ptrs(source_addrs, PAGE_SIZE / self.supported_block_size)?
                .write(destination_addr, total_num_blocks)
        ).await;
        match (cmp.status.status_code_type(), cmp.status.status_code()) {
            (0, 0) => Ok(total_num_blocks as usize),
            _ => {
                // TODO: this could maybe be more specific
                Err(StorageError::DeviceError {
                    reason: format!("failed to do NVMe write from {source_addrs:?} of size {total_num_blocks} to {destination_addr}: {cmp:?}")
                })
            }
        }
    }
}
