//! Support for the FAT filesystem for QEMU
//! Currently this only supports FAT16.
// See <qemu-src>/block/vvfat.c

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use async_recursion::async_recursion;
use async_trait::async_trait;
use bitfield::bitfield;
use byteorder::{ByteOrder, LittleEndian};
use futures::{Stream, StreamExt};
use smallvec::SmallVec;
use snafu::{ensure, OptionExt, ResultExt};
use widestring::Utf16Str;

use crate::{
    fs::{BadMetadataSnafu, OtherSnafu},
    registry::{registry_mut, Path, RegistryHandler},
    storage::{block_cache::BlockCache, BlockAddress, BlockStore},
};

use super::{ByteStore, Error, SeekFrom, StorageSnafu};

mod data;
use data::*;

const CACHE_SIZE: usize = 256 /* pages */;

// assume that every FAT filesystem uses 512 byte sectors
const SECTOR_SIZE: u64 = 512;

struct File {
    cache: Arc<BlockCache>,
    params: VolumeParams,
    start_cluster_number: u16,
    current_cluster_number: u16,
    current_offset: usize,
    file_size: u32
}

#[async_trait]
impl ByteStore for File {
    async fn seek(&mut self, pos: SeekFrom) -> Result<u64, Error> {
        todo!()
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        todo!()
    }

    async fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        todo!()
    }
}

#[derive(Debug, Clone)]
struct VolumeParams {
    fat_start: BlockAddress,
    clusters_start: BlockAddress,
    root_directory_start: BlockAddress,
    sectors_per_cluster: u64,
}

impl VolumeParams {
    pub fn compute_from_bootsector(partition_addr: BlockAddress, bootsector: &BootSector) -> Self {
        Self {
            fat_start: BlockAddress(partition_addr.0 + bootsector.reserved_sectors_count as u64),
            // TODO: are these supposed to be the same???
            clusters_start: BlockAddress(
                partition_addr.0
                    + bootsector.reserved_sectors_count as u64
                    + bootsector.number_of_fats as u64 * bootsector.fat_size_16 as u64,
            ),
            root_directory_start: BlockAddress(
                partition_addr.0
                    + bootsector.reserved_sectors_count as u64
                    + (bootsector.number_of_fats as u64 * bootsector.fat_size_16 as u64),
            ),
            sectors_per_cluster: bootsector.sectors_per_cluster as u64,
        }
    }
}

struct Handler {
    cache: Arc<BlockCache>,
    params: VolumeParams,
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

        let mut hh = Handler {
            cache: Arc::new(cache),
            params: VolumeParams::compute_from_bootsector(partition_addr, bootsector),
        };
        log::debug!("volume parameters = {:?}", hh.params);

        hh.dir_entry_stream(DirectorySource::Direct(hh.params.root_directory_start))
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
         *     + read directory in clusters
         *     + recursively go through directories to find actual file
         * - read file (implement ByteStore) */

        Ok(hh)
    }
}

fn compute_block_addr(source: &DirectorySource, params: &VolumeParams) -> BlockAddress {
    match source {
        DirectorySource::Direct(a) => *a,
        DirectorySource::Clusters(i) => {
            BlockAddress(params.clusters_start.0 + *i as u64 * params.sectors_per_cluster)
        }
    }
}

impl Handler {
    fn dir_entry_stream(
        &self,
        source: DirectorySource,
    ) -> impl Stream<Item = Result<LongDirEntry, Error>> + '_ {
        // max # of bytes in offset before wrapping and moving to the next segment
        let size_of_segment = match source {
            DirectorySource::Direct(_) => self.cache.block_size(),
            DirectorySource::Clusters(_) => {
                (self.params.sectors_per_cluster * SECTOR_SIZE) as usize
            }
        };
        futures::stream::unfold(
            (&self.cache, source, 0),
            move |(cache, mut source, mut offset)| {
                let params = self.params.clone();
                async move {
                    let mut entry_bytes = [0u8; core::mem::size_of::<DirEntry>()];
                    let mut entry_name = SmallVec::new();

                    let mut block_addr = compute_block_addr(&source, &params);

                    loop {
                        match cache
                            .copy_bytes(block_addr, offset, &mut entry_bytes)
                            .await
                            .context(StorageSnafu)
                        {
                            Ok(_) => {}
                            Err(e) => return Some((Err(e), (cache, source, offset))),
                        }

                        log::trace!("{:?}", bytemuck::cast_ref::<_, DirEntry>(&entry_bytes));

                        offset += core::mem::size_of::<DirEntry>();
                        if offset >= size_of_segment {
                            offset = 0;
                            log::trace!("current src={source:?}");
                            match &mut source {
                                DirectorySource::Direct(ref mut d) => d.0 += 1,
                                DirectorySource::Clusters(ref mut i) => {
                                    match cache
                                        .copy_bytes(
                                            params.fat_start,
                                            (*i * 2) as usize,
                                            bytemuck::bytes_of_mut(i),
                                        )
                                        .await
                                        .context(StorageSnafu)
                                    {
                                        Ok(_) => {}
                                        Err(e) => return Some((Err(e), (cache, source, offset))),
                                    }
                                }
                            }
                            block_addr = compute_block_addr(&source, &params);
                            log::trace!("src={source:?}; next block address: {block_addr}");
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
                            if entry_name.last().is_some_and(|t| *t == 0) {
                                entry_name.pop();
                                entry_name.shrink_to_fit();
                            }
                            return Some((
                                Ok(LongDirEntry {
                                    entry: bytemuck::cast(entry_bytes),
                                    name: entry_name,
                                }),
                                (cache, source, offset),
                            ));
                        }
                    }
                }
            },
        )
    }

    #[async_recursion]
    async fn find_entry_for_path(
        &self,
        source: DirectorySource,
        mut comps: crate::registry::path::Components<'async_recursion>,
    ) -> Result<Option<DirEntry>, Error> {
        let comp = comps.next().unwrap();
        log::trace!("find entry for path component {comp:?} in {source:?}");
        let ps = match comp {
            crate::registry::path::Component::Root => todo!(),
            crate::registry::path::Component::CurrentDir => ".",
            crate::registry::path::Component::ParentDir => "..",
            crate::registry::path::Component::Name(s) => s,
        };

        let v = self.dir_entry_stream(source).collect::<Vec<_>>().await;

        for de in v {
            let de = de?;
            log::trace!("got entry {:?}, {}", de.long_name(), unsafe {
                core::str::from_utf8_unchecked(&de.short_name)
            });
            if de
                .is_named(ps)
                .map_err(|e| Box::new(e) as Box<dyn snafu::Error + Send + Sync>)
                .context(OtherSnafu {
                    reason: "check entry name",
                })?
            {
                if de.attributes.contains(DirEntryAttributes::Directory) {
                    return self
                        .find_entry_for_path(DirectorySource::Clusters(de.cluster_num_lo), comps)
                        .await;
                } else {
                    return Ok(Some(*de));
                }
            }
        }

        Ok(None)
    }
}

#[async_trait]
impl RegistryHandler for Handler {
    async fn open_block_store(
        &self,
        subpath: &Path,
    ) -> Result<Box<dyn BlockStore>, crate::registry::RegistryError> {
        Err(crate::registry::error::UnsupportedSnafu.build())
    }

    async fn open_byte_store(
        &self,
        subpath: &Path,
    ) -> Result<Box<dyn super::ByteStore>, crate::registry::RegistryError> {
        log::trace!("attempting to locate {subpath} in FAT volume");

        let entry = self
            .find_entry_for_path(
                DirectorySource::Direct(self.params.root_directory_start),
                subpath.components(),
            )
            .await
            .map_err(|e| Box::new(e) as Box<dyn snafu::Error + Send + Sync>)
            .context(crate::registry::error::OtherSnafu {
                reason: "failed to find directory entry for path",
            })?
            .with_context(|| crate::registry::error::NotFoundSnafu { path: subpath })?;

        log::debug!("found entry {entry:?} for path {subpath}");

        Ok(Box::new(File {
            cache: self.cache.clone(),
            params: self.params.clone(),
            start_cluster_number: entry.cluster_num_lo,
            current_cluster_number: entry.cluster_num_lo,
            current_offset: 0,
            file_size: entry.file_size
        }))
    }
}

/// Mount a FAT filesystem present on `block_store` under `root_path` in the registry.
pub async fn mount(root_path: &Path, block_store: Box<dyn BlockStore>) -> Result<(), Error> {
    registry_mut()
        .register(root_path, Box::new(Handler::new(block_store).await?))
        .context(super::RegistrySnafu)
}
