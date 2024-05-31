//! Support for the FAT filesystem for QEMU
//! Currently this only supports FAT16.
// See <qemu-src>/block/vvfat.c
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use async_recursion::async_recursion;
use async_trait::async_trait;

use bytemuck::bytes_of_mut;
use futures::{Stream, StreamExt, TryStream, TryStreamExt};
use kapi::FileUSize;
use smallvec::SmallVec;
use snafu::{ensure, OptionExt, ResultExt};

use crate::{
    error::{self, Error},
    fs::BadMetadataSnafu,
    registry::{self, registry_mut, Path, RegistryHandler},
    storage::{block_cache::BlockCache, BlockAddress, BlockStore},
};

use super::{File, FsError};

mod data;
use data::*;

const CACHE_SIZE: usize = 512 /* pages */;

#[derive(Debug, Clone)]
struct VolumeParams {
    fat_start: BlockAddress,
    clusters_start: BlockAddress,
    root_directory_start: BlockAddress,
    sectors_per_cluster: u64,
    bytes_per_sector: u64,
}

impl VolumeParams {
    fn compute_from_bootsector(partition_addr: BlockAddress, bootsector: &BootSector) -> Self {
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
            bytes_per_sector: bootsector.bytes_per_sector as u64,
        }
    }

    const fn sector_for_cluster(&self, cluster_num: ClusterIndex) -> BlockAddress {
        BlockAddress(self.clusters_start.0 + cluster_num as u64 * self.sectors_per_cluster)
    }

    const fn bytes_per_cluster(&self) -> u64 {
        self.sectors_per_cluster * self.bytes_per_sector
    }
}

struct FatFs {
    cache: BlockCache,
    params: VolumeParams,
}

impl FatFs {
    async fn new(block_store: Box<dyn BlockStore>) -> Result<FatFs, Error> {
        let cache = BlockCache::new(block_store, CACHE_SIZE)?;

        // read the relevant part of the MBR, starting at byte 446
        let mut mbr_data = [0u8; 66];
        cache
            .copy_bytes(BlockAddress(0), 446, &mut mbr_data)
            .await
            .context(error::StorageSnafu { reason: "load MBR" })?;

        // check MBR magic bytes
        if mbr_data[64] != 0x55 && mbr_data[65] != 0xaa {
            return Err(FsError::BadMetadata {
                message: "bad MBR magic bytes",
                value: ((mbr_data[64] as usize) << 8) | mbr_data[65] as usize,
            })
            .context(error::FileSystemSnafu {
                reason: "check MBR magic",
            });
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
            .context(error::StorageSnafu {
                reason: "read boot sector",
            })?;
        log::debug!("{bootsector_data:x?}");

        // check boot sector magic bytes
        if !(bootsector_data[510] == 0x55 && bootsector_data[511] == 0xaa) {
            return Err(BadMetadataSnafu {
                message: "invalid boot sector signature",
                value: bootsector_data[510],
            }
            .build())
            .context(error::FileSystemSnafu {
                reason: "check boot sector magic",
            });
        }

        let bootsector: &BootSector = unsafe { &*(bootsector_data.as_ptr() as *const BootSector) };
        log::debug!("FAT bootsector = {bootsector:x?}");
        bootsector.validate().context(error::FileSystemSnafu {
            reason: "validate boot sector",
        })?;

        let hh = FatFs {
            cache,
            params: VolumeParams::compute_from_bootsector(partition_addr, bootsector),
        };
        log::debug!("volume parameters = {:?}", hh.params);

        // debug log the root directory
        /*hh.dir_entry_stream(DirectorySource::Direct(hh.params.root_directory_start))
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
        .await;*/

        /* TODO:
         * - traverse directories to locate a file
         *     + read root directory
         *     + read directory in clusters
         *     + recursively go through directories to find actual file
         * - read file (implement ByteStore) */

        Ok(hh)
    }

    /// Create a mapping <cluster index> -> (<buffer index>, <buffer offset>) from a vector
    /// descriptor iterator (<buffer index>, <buffer length>) and the number of clusters total.
    /// The mapping maps clusters to the *first* buffer they would affect.
    fn cluster_to_buffer_vector_map(
        &self,
        mut buffer_vector: impl Iterator<Item = (usize, usize)>,
        start_offset: usize,
        num_clusters: usize,
    ) -> Vec<(usize, usize)> {
        let mut map = Vec::with_capacity(num_clusters);
        let (mut current_buffer_index, mut current_buffer_length) =
            buffer_vector.next().expect("buffer vector is non-empty");
        let mut current_offset = 0;
        let cluster_size = self.params.bytes_per_cluster() as usize;
        for cluster in 0..num_clusters {
            map.push((current_buffer_index, current_offset));
            // log::trace!("{cluster} => {current_buffer_index} + {current_offset}");
            let mut src_offset = if cluster == 0 { start_offset } else { 0 };
            loop {
                let src_remaining = cluster_size - src_offset;
                let dst_remaining = current_buffer_length - current_offset;
                let len = src_remaining.min(dst_remaining);
                current_offset += len;
                if current_offset == current_buffer_length {
                    current_offset = 0;
                    match buffer_vector.next() {
                        Some(d) => (current_buffer_index, current_buffer_length) = d,
                        None => break,
                    }
                }
                src_offset += len;
                if src_offset == cluster_size {
                    // we're finished with this cluster, move to the next one
                    break;
                }
            }
        }
        map
    }

    /// Determine if the cluster number is a special value representing the end of the chain.
    fn cluster_number_is_end(&self, cluster_number: ClusterIndex) -> bool {
        // TODO: this only works for FAT16.
        cluster_number >= 0xfff8
    }

    /// Create a stream of block addresses locating the start of each cluster in a cluster chain
    /// found in the FAT, and the relative cluster index from the start of the chain.
    fn cluster_address_stream(
        &self,
        start_cluster_number: ClusterIndex,
    ) -> impl TryStream<Ok = (BlockAddress, usize), Error = Error> + '_ {
        futures::stream::try_unfold(
            (start_cluster_number, 0),
            move |(current_cluster_number, current_cluster_index)| async move {
                if self.cluster_number_is_end(current_cluster_number) {
                    Ok(None)
                } else {
                    let current_cluster_address =
                        self.params.sector_for_cluster(current_cluster_number);
                    let mut next_cluster_number = ClusterIndex::MAX;
                    self.cache
                        .copy_bytes(
                            self.params.fat_start,
                            (current_cluster_number * 2) as usize,
                            bytes_of_mut(&mut next_cluster_number),
                        )
                        .await
                        .context(error::StorageSnafu {
                            reason: "read FAT table for next cluster",
                        })?;
                    Ok(Some((
                        (current_cluster_address, current_cluster_index),
                        (next_cluster_number, current_cluster_index + 1),
                    )))
                }
            },
        )
    }

    // fn dir_entry_stream2(&self, start_cluster_number: u16) -> impl TryStream<Ok = LongDirEntry, Error = Error> + '_ {
    //     self.cluster_address_stream(start_cluster_number)
    //         .and_then(|block_addr| async move {
    //             let mut block = Vec::with_capacity(self.params.bytes_per_cluster() as usize);
    //             block.resize(self.params.bytes_per_cluster() as usize, 0);
    //             self.cache.copy_bytes(block_addr, 0, &mut block)
    //                 .await
    //                 .context(error::StorageSnafu {
    //                     reason: "read directory block"
    //                 })?;
    //             todo!()
    //         })
    //         .try_flatten()
    // }

    fn dir_entry_stream(
        &self,
        source: DirectorySource,
    ) -> impl Stream<Item = Result<LongDirEntry, Error>> + '_ {
        fn compute_block_addr(source: &DirectorySource, params: &VolumeParams) -> BlockAddress {
            match source {
                DirectorySource::Direct(a) => *a,
                DirectorySource::Clusters(i) => params.sector_for_cluster(*i),
            }
        }

        // max # of bytes in offset before wrapping and moving to the next segment
        let size_of_segment = match source {
            DirectorySource::Direct(_) => self.cache.block_size(),
            DirectorySource::Clusters(_) => self.params.bytes_per_cluster() as usize,
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
                        // read a directory entry
                        match cache
                            .copy_bytes(block_addr, offset, &mut entry_bytes)
                            .await
                            .context(error::StorageSnafu {
                                reason: "read directory entry",
                            }) {
                            Ok(_) => {}
                            Err(e) => return Some((Err(e), (cache, source, offset))),
                        }

                        // log::trace!("{:?}", bytemuck::cast_ref::<_, DirEntry>(&entry_bytes));

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
                                        .context(error::StorageSnafu {
                                            reason: "read next cluster in FAT",
                                        }) {
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
                        }
                        if entry_bytes[11] == DirEntryAttributes::LongName.bits() {
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
            match de.is_named(ps) {
                Ok(true) => {
                    if de.attributes.contains(DirEntryAttributes::Directory) {
                        return self
                            .find_entry_for_path(
                                DirectorySource::Clusters(de.cluster_num_lo),
                                comps,
                            )
                            .await;
                    }
                    return Ok(Some(*de));
                }
                Ok(false) => (),
                Err(e) => {
                    log::warn!("failed to read directory entry: {e}");
                    // just ignore the entry, this isn't fatal
                }
            }
        }

        Ok(None)
    }
}

struct FatFile {
    fs: Arc<FatFs>,
    start_cluster_number: ClusterIndex,
    file_size: u32,
}

impl FatFile {
    /// Returns a stream of (cluster index from beginning of file, address of starting block in cluster)
    /// over the range of clusters in start..end also relative to the start of the file
    fn cluster_slice_stream(
        &self,
        start_cluster: usize,
        end_cluster: usize,
    ) -> impl TryStream<Ok = (BlockAddress, usize), Error = Error> + '_ {
        self.fs
            .cluster_address_stream(self.start_cluster_number)
            .try_skip_while(move |(_, i)| futures::future::ok(*i < start_cluster))
            .try_take_while(move |(_, i)| futures::future::ok(*i < end_cluster))
    }
}

#[async_trait]
impl File for FatFile {
    fn len(&self) -> FileUSize {
        self.file_size as FileUSize
    }

    /// Read bytes starting from `src_offset` in the file into the buffers in `destinations`.
    /// The total length of `destinations` must fit within the file or the read will fail with an OutOfBounds error.
    /// No IO will occur in this case.
    async fn read<'v, 'dest>(
        &self,
        src_offset: FileUSize,
        destinations: &'v mut [&'dest mut [u8]],
    ) -> Result<(), Error> {
        /// This is basically a &[&mut [u8]] slice, except you can get a mutable reference to any interior slice.
        /// This is safe because we are only going to interact with only one interior slice at a time,
        /// but we have to look sharable to be able to move into the async closure.
        /// Since we construct this from a &mut rather than a &, we know that we have exclusive
        /// access to the slice pointers, so it shouldn't be too big of a deal.
        /// This is obviously only safe to use inside this function where we know exactly what will
        /// happen to it.
        #[derive(Copy, Clone)]
        struct ReadDestSlice<'dst> {
            ptr: *const &'dst mut [u8],
            len: usize,
        }

        impl<'dst> From<&mut [&'dst mut [u8]]> for ReadDestSlice<'dst> {
            fn from(value: &mut [&'dst mut [u8]]) -> Self {
                Self {
                    ptr: value.as_ptr(),
                    len: value.len(),
                }
            }
        }

        impl<'dst> ReadDestSlice<'dst> {
            fn get(&self, index: usize) -> &'dst mut [u8] {
                assert!(index < self.len);
                unsafe { self.ptr.add(index).read() }
            }
        }

        unsafe impl Send for ReadDestSlice<'_> {}

        let total_read_size: FileUSize = destinations.iter().map(|s| s.len() as FileUSize).sum();

        if (src_offset + total_read_size) > self.len() {
            return Err(Error::FileSystem {
                reason: "read is out of bounds".into(),
                source: Box::new(FsError::OutOfBounds {
                    value: src_offset + total_read_size,
                    bound: self.len(),
                    message: "read",
                }),
            });
        }

        // compute the range of clusters we need to read
        let start_cluster = src_offset.div_floor(self.fs.params.bytes_per_sector) as usize;
        let start_offset = (src_offset % self.fs.params.bytes_per_sector) as usize;
        let cluster_size = self.fs.params.bytes_per_cluster() as usize;
        let num_clusters = (total_read_size as usize).div_ceil(cluster_size);
        let end_cluster = start_cluster + num_clusters;

        log::trace!("read: start_cluster={start_cluster}, start_offset={start_offset}, end_cluster={end_cluster}");

        let cluster_map = &self.fs.cluster_to_buffer_vector_map(
            destinations.iter().map(|s| s.len()).enumerate(),
            start_offset,
            num_clusters,
        );

        let dsts = ReadDestSlice::from(destinations);

        self.cluster_slice_stream(start_cluster, end_cluster)
            .try_for_each(move |(cluster_addr, cluster_index)| async move {
                let (mut current_dst, mut current_dst_offset) = cluster_map[cluster_index];
                let mut src_offset = if cluster_index == 0 {
                    start_offset
                } else {
                    0
                };
                loop {
                    let dst_buf = dsts.get(current_dst);
                    let src_remaining = cluster_size - src_offset;
                    let dst_remaining = dst_buf.len() - current_dst_offset;
                    let len = src_remaining.min(dst_remaining);
                    log::trace!("copy bytes from ({cluster_index}){cluster_addr}+{src_offset} to {current_dst}.{current_dst_offset} ({len} bytes)");
                    self.fs
                        .cache
                        .copy_bytes(
                            cluster_addr,
                            src_offset,
                            &mut dst_buf[current_dst_offset..current_dst_offset + len],
                        )
                        .await
                        .context(error::StorageSnafu {
                            reason: "copy bytes from file",
                        })?;
                    current_dst_offset += len;
                    if current_dst_offset == dst_buf.len() {
                        current_dst_offset = 0;
                        current_dst += 1;
                        if current_dst >= dsts.len {
                            // we're done reading the file, this was the last cluster
                            break Ok(());
                        }
                    }
                    src_offset += len;
                    if src_offset == cluster_size {
                        // we're finished with this cluster, move to the next one
                        break Ok(());
                    }
                }
            })
            .await
    }

    /// Write bytes starting at `dst_offset` in the file from the buffers in `sources`.
    /// The total length of `sources` must fit within the file or the write will fail with an OutOfBounds error.
    /// No IO will occur in this case.
    async fn write<'v, 'src>(
        &mut self,
        _dst_offset: FileUSize,
        _sources: &'v [&'src [u8]],
    ) -> Result<(), Error> {
        todo!()
    }
}

struct Handler {
    fs: Arc<FatFs>,
}

#[async_trait]
impl RegistryHandler for Handler {
    async fn open_file(&self, subpath: &Path) -> Result<Box<dyn super::File>, Error> {
        log::trace!("attempting to locate {subpath} in FAT volume");

        let entry = self
            .fs
            .find_entry_for_path(
                DirectorySource::Direct(self.fs.params.root_directory_start),
                subpath.components(),
            )
            .await?
            .context(registry::NotFoundSnafu { path: subpath })
            .context(error::RegistrySnafu {
                reason: "failed to find directory entry for path",
            })?;

        log::debug!("found entry {entry:?} for path {subpath}");

        Ok(Box::new(FatFile {
            fs: self.fs.clone(),
            start_cluster_number: entry.cluster_num_lo,
            file_size: entry.file_size,
        }))
    }
}

/// Mount a FAT filesystem present on `block_store` under `root_path` in the registry.
pub async fn mount(root_path: &Path, block_store: Box<dyn BlockStore>) -> Result<(), Error> {
    registry_mut()
        .register(
            root_path,
            Box::new(Handler {
                fs: Arc::new(FatFs::new(block_store).await?),
            }),
        )
        .context(error::RegistrySnafu {
            reason: "register FAT filesystem",
        })
}
