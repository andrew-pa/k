use alloc::boxed::Box;
use async_trait::async_trait;
use byteorder::{ByteOrder, LittleEndian};
use snafu::ResultExt;

use crate::{
    registry::{registry_mut, Path, RegistryHandler},
    storage::{block_cache::BlockCache, BlockStore, LogicalAddress},
};

use super::Error;

const CACHE_SIZE: usize = 256 /* pages */;

struct Handler {}

impl Handler {
    async fn new(block_store: Box<dyn BlockStore>) -> Result<Handler, Error> {
        let mut cache = BlockCache::new(block_store, CACHE_SIZE).context(super::StorageSnafu)?;

        // read the MBR
        let mut mbr_data = [0u8; 66];
        cache
            .copy_bytes(LogicalAddress(0), 446, &mut mbr_data)
            .await
            .context(super::StorageSnafu)?;

        // check MBR magic bytes
        if mbr_data[64] != 0x55 && mbr_data[65] != 0xaa {
            return Err(todo!("bad mbr"));
        }

        log::debug!("{mbr_data:x?}");

        for i in 0..4 {
            let type_code = mbr_data[i * 16 + 5];
            let partition_addr =
                LogicalAddress(LittleEndian::read_u32(&mbr_data[i * 16 + 8..]) as u64);
            log::debug!("FAT partition at {partition_addr}, type={type_code:x}");
        }

        // TODO: handle more than one partition in slot 0
        let partition_addr = LogicalAddress(LittleEndian::read_u32(&mbr_data[8..]) as u64);
        log::debug!("using partition at {partition_addr}");
        assert!(partition_addr.0 > 0);
        let mut vol_id_data = [0u8; 512];
        cache
            .copy_bytes(partition_addr, 0, &mut vol_id_data)
            .await
            .context(super::StorageSnafu)?;
        log::debug!("{vol_id_data:x?}");

        // check volume ID magic bytes
        if vol_id_data[510] != 0x55 && vol_id_data[511] != 0xaa {
            return Err(todo!("bad volume id"));
        }

        todo!()
    }
}

#[async_trait]
impl RegistryHandler for Handler {
    async fn open_block_store(
        &self,
        subpath: &Path,
    ) -> Result<Box<dyn BlockStore>, crate::registry::RegistryError> {
        todo!()
    }

    async fn open_byte_store(
        &self,
        subpath: &Path,
    ) -> Result<Box<dyn super::ByteStore>, crate::registry::RegistryError> {
        todo!()
    }
}

/// Mount a FAT filesystem present on `block_store` under `root_path` in the registry.
pub async fn mount(root_path: &Path, block_store: Box<dyn BlockStore>) -> Result<(), Error> {
    registry_mut()
        .register(root_path, Box::new(Handler::new(block_store).await?))
        .context(super::RegistrySnafu)
}
