//! Support for the FAT filesystem for QEMU
//! Currently this only supports FAT16.
// See <qemu-src>/block/vvfat.c

use alloc::boxed::Box;
use async_trait::async_trait;
use bitfield::bitfield;
use byteorder::{ByteOrder, LittleEndian};
use futures::{Stream, StreamExt};
use smallvec::SmallVec;
use snafu::{ensure, ResultExt};

use crate::{
    fs::BadMetadataSnafu,
    registry::{registry_mut, Path, RegistryHandler},
    storage::{block_cache::BlockCache, BlockAddress, BlockStore},
};

use super::{Error, StorageSnafu};

const CACHE_SIZE: usize = 256 /* pages */;

// assume that every FAT filesystem uses 512 byte sectors
const SECTOR_SIZE: u64 = 512;

mod data;
use data::*;

struct Handler {
    cache: BlockCache,
    fat_start: BlockAddress,
    clusters_start: BlockAddress,
    root_directory_start: BlockAddress,
    sectors_per_cluster: u64,
}

impl Handler {
    async fn new(block_store: Box<dyn BlockStore>) -> Result<Handler, Error> {
        let mut cache = BlockCache::new(block_store, CACHE_SIZE).context(super::StorageSnafu)?;

        // read the relevant part of the MBR, starting at byte 446
        let mut mbr_data = [0u8; 66];
        cache
            .copy_bytes(BlockAddress(0), 446, &mut mbr_data)
            .await
            .context(super::StorageSnafu)?;

        // check MBR magic bytes
        if mbr_data[64] != 0x55 && mbr_data[65] != 0xaa {
            return Err(todo!("bad mbr"));
        }

        log::debug!("MBR: {mbr_data:x?}");

        let partition_table: &[PartitionEntry] =
            unsafe { core::slice::from_raw_parts(mbr_data.as_ptr() as *const PartitionEntry, 4) };

        for p in partition_table {
            log::debug!("FAT partition {p:x?}");
        }

        // TODO: handle more than one partition in slot 0
        let partition_addr = BlockAddress(partition_table[0].start_sector.into());
        log::debug!("using partition at {partition_addr}");
        assert!(partition_addr.0 > 0);
        let mut bootsector_data = [0u8; 512];
        cache
            .copy_bytes(partition_addr, 0, &mut bootsector_data)
            .await
            .context(super::StorageSnafu)?;
        log::debug!("{bootsector_data:x?}");

        // check boot sector magic bytes
        ensure!(
            bootsector_data[510] == 0x55 && bootsector_data[511] == 0xaa,
            BadMetadataSnafu {
                message: "invalid boot sector signature",
                value: bootsector_data[510]
            }
        );
        let bootsector: &BootSector = unsafe { &*(bootsector_data.as_ptr() as *const BootSector) };
        log::debug!("FAT bootsector = {bootsector:x?}");
        bootsector.validate()?;

        let root_dir_sector = BlockAddress(
            partition_addr.0
                + bootsector.reserved_sectors_count as u64
                + (bootsector.number_of_fats as u64 * bootsector.fat_size_16 as u64),
        );

        let mut hh = Handler {
            cache,
            fat_start: BlockAddress(partition_addr.0 + bootsector.reserved_sectors_count as u64),
            clusters_start: BlockAddress(
                partition_addr.0
                    + bootsector.reserved_sectors_count as u64
                    + bootsector.number_of_fats as u64 * bootsector.fat_size_16 as u64,
            ),
            root_directory_start: root_dir_sector,
            sectors_per_cluster: bootsector.sectors_per_cluster as u64,
        };

        hh.dir_entry_stream(DirectorySource::Direct(root_dir_sector))
            .for_each(|e| async move {
                match e {
                    Ok(e) => {
                        let c = e.cluster_num_lo;
                        let sz = e.file_size;
                        log::debug!(
                            "{:?} {} : {} @ {c:x} + {sz}",
                            e.attributes,
                            unsafe { core::str::from_utf8_unchecked(&e.short_name) },
                            e.long_name().unwrap(),
                        )
                    }
                    Err(e) => log::error!("failed to read root directory entry: {e}"),
                }
            })
            .await;

        /* TODO:
         * - traverse directories to locate a file
         *     + read root directory
         *     - read directory in clusters
         *     - recursively go through directories to find actual file
         * - read file (implement ByteStore) */

        todo!();

        Ok(hh)
    }
}

fn compute_block_addr(
    source: &DirectorySource,
    clusters_start: BlockAddress,
    sectors_per_cluster: u64,
) -> BlockAddress {
    match source {
        DirectorySource::Direct(a) => *a,
        DirectorySource::Clusters(i) => {
            BlockAddress(clusters_start.0 + (i - 2) as u64 * sectors_per_cluster)
        }
    }
}

impl Handler {
    fn dir_entry_stream(
        &mut self,
        source: DirectorySource,
    ) -> impl Stream<Item = Result<LongDirEntry, Error>> + '_ {
        // max # of bytes in offset before wrapping and moving to the next segment
        let size_of_segment = match source {
            DirectorySource::Direct(_) => self.cache.block_size(),
            DirectorySource::Clusters(_) => (self.sectors_per_cluster * SECTOR_SIZE) as usize,
        };
        let fat_start = self.fat_start;
        let clusters_start = self.clusters_start;
        let sectors_per_cluster = self.sectors_per_cluster;
        futures::stream::unfold(
            (&mut self.cache, source, 0),
            move |(cache, mut source, mut offset)| async move {
                let mut entry_bytes = [0u8; core::mem::size_of::<DirEntry>()];
                let mut entry_name = SmallVec::new();

                let mut block_addr =
                    compute_block_addr(&source, clusters_start, sectors_per_cluster);

                loop {
                    match cache
                        .copy_bytes(block_addr, offset, &mut entry_bytes)
                        .await
                        .context(StorageSnafu)
                    {
                        Ok(_) => {}
                        Err(e) => return Some((Err(e), (cache, source, offset))),
                    }

                    offset += core::mem::size_of::<DirEntry>();
                    if offset >= size_of_segment {
                        offset = 0;
                        match source {
                            DirectorySource::Direct(mut d) => d.0 += 1,
                            DirectorySource::Clusters(mut i) => {
                                match cache
                                    .copy_bytes(
                                        fat_start,
                                        (i * 2) as usize,
                                        bytemuck::bytes_of_mut(&mut i),
                                    )
                                    .await
                                    .context(StorageSnafu)
                                {
                                    Ok(_) => {}
                                    Err(e) => return Some((Err(e), (cache, source, offset))),
                                }
                            }
                        }
                        block_addr =
                            compute_block_addr(&source, clusters_start, sectors_per_cluster);
                    }

                    if entry_bytes[0] == 0 {
                        return None;
                    } else if entry_bytes[11] == DirEntryAttributes::LongName.bits() {
                        entry_name.insert_many(
                            0,
                            (1..11)
                                .chain(14..26)
                                .chain(28..32)
                                .map(|i| entry_bytes[i])
                                .take_while(|b| *b != 0xff)
                                .array_chunks()
                                .map(|a: [u8; 2]| bytemuck::cast(a)),
                        );
                    } else {
                        return Some((
                            Ok(LongDirEntry {
                                entry: bytemuck::cast(entry_bytes),
                                name: entry_name,
                            }),
                            (cache, source, offset),
                        ));
                    }
                }
            },
        )
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
