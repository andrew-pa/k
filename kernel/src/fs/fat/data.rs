//! Data structures for FAT filesystems
use core::ops::Deref;

use bitflags::bitflags;
use bytemuck::{bytes_of, bytes_of_mut, from_bytes, Pod, Zeroable};
use futures::Stream;
use smallvec::SmallVec;
use widestring::Utf16Str;

use crate::fs::StorageSnafu;

use super::*;

/// A entry in the partition table found in the MBR.
#[repr(C, packed)]
#[derive(Debug, Copy, Clone, Zeroable, Pod)]
pub struct PartitionEntry {
    pub attributes: u8,
    pub start_chs: [u8; 3],
    pub fs_type: u8,
    pub end_chs: [u8; 3],
    pub start_sector: u32,
    pub length: u32,
}

/// The FAT boot sector/"BIOS parameter block" data structure.
#[repr(C, packed)]
#[derive(Debug, Copy, Clone, Zeroable, Pod)]
pub struct BootSector {
    /// BS_jmpBoot
    pub jump_boot: [u8; 3],
    /// BS_OEMName
    pub oem_name: [u8; 8],
    /// BPB_BytsPerSec
    pub bytes_per_sector: u16,
    /// BPB_SecPerClus
    pub sectors_per_cluster: u8,
    /// BPB_RsvdSecCnt
    pub reserved_sectors_count: u16,
    /// BPB_NumFATs
    pub number_of_fats: u8,
    /// BPB_RootEntCnt
    pub root_entries_count: u16,
    /// BPB_TotSec16
    pub total_sectors_16: u16,
    /// BPB_Media
    pub media: u8,
    /// BPB_FATSz16
    pub fat_size_16: u16,
    /// BPB_SecPerTrk
    pub sectors_per_track: u16,
    /// BPB_NumHeads
    pub number_of_heads: u16,
    /// BPB_HiddSec
    pub hidden_sectors: u32,
    /// BPB_TotSec32
    pub total_sectors_32: u32,

    // Fat12 and Fat16 Structure Starting at Offset 36
    /// BS_DrvNum
    pub drive_number: u8,
    /// BS_Reserved1
    pub reserved: u8,
    /// BS_BootSig
    pub boot_signature: u8,
    /// BS_VolID
    pub volume_id: u32,
    /// BS_VolLab
    pub volume_label: [u8; 11],
    /// BS_FilSysType
    pub file_system_type: [u8; 8],
}

impl BootSector {
    /// Check to make sure that this partition has a valid and supported boot sector.
    pub fn validate(&self) -> Result<(), Error> {
        ensure!(
            self.bytes_per_sector as u64 == super::SECTOR_SIZE,
            BadMetadataSnafu {
                message: "bytes per sector != 512",
                value: self.bytes_per_sector
            }
        );
        ensure!(
            self.number_of_fats == 2,
            BadMetadataSnafu {
                message: "number of FATs != 2",
                value: self.number_of_fats
            }
        );
        // TODO: make sure that this is FAT16 volume since that's all that is currently supported
        Ok(())
    }
}

bitflags! {
    #[derive(Copy, Clone, PartialEq, Eq, Debug, Zeroable, Pod)]
    #[repr(C)]
    pub struct DirEntryAttributes : u8 {
        /// ATTR_READ_ONLY
        const ReadOnly = 0x01;
        /// ATTR_HIDDEN
        const Hidden = 0x02;
        /// ATTR_SYSTEM
        const System = 0x04;
        /// ATTR_VOLUME_ID
        const VolumeId = 0x08;
        /// ATTR_DIRECTORY
        const Directory = 0x10;
        /// ATTR_ARCHIVE
        const Archive = 0x20;
        /// ATTR_LONG_NAME
        const LongName = Self::ReadOnly.bits() | Self::Hidden.bits() | Self::System.bits() | Self::VolumeId.bits();
    }
}

#[repr(C, packed)]
#[derive(Debug, Copy, Clone, Zeroable, Pod)]
pub struct DirEntry {
    /// DIR_Name
    pub short_name: [u8; 11],
    /// DIR_Attr
    pub attributes: DirEntryAttributes,
    /// DIR_NTRes (set to 0)
    pub res: u8,
    /// DIR_CrtTimeTenth
    pub creation_time_tenths_of_second: u8,

    /// DIR_???
    pub creation_time: u16,
    /// DIR_???
    pub creation_date: u16,
    /// DIR_???
    pub last_access_date: u16,

    /// DIR_FstClusHI
    pub cluster_num_hi: u16,
    /// DIR_WrtTime
    pub last_write_time: u16,
    /// DIR_WrtDate
    pub last_write_date: u16,
    /// DIR_FstClusLO
    pub cluster_num_lo: u16,
    /// DIR_FileSize
    pub file_size: u32,
}

pub fn dir_entry_stream(
    cache: &mut BlockCache,
    dir_start_address: BlockAddress,
) -> impl Stream<Item = Result<DirEntry, Error>> + '_ {
    futures::stream::unfold(
        (cache, dir_start_address, 0),
        move |(cache, mut block_addr, mut offset)| async move {
            let mut de = DirEntry::zeroed();
            match cache
                .copy_bytes(block_addr, offset, bytes_of_mut(&mut de))
                .await
                .context(StorageSnafu)
            {
                Ok(_) => {}
                Err(e) => return Some((Err(e), (cache, block_addr, offset))),
            }
            if de.short_name[0] == 0 {
                return None;
            }
            offset += core::mem::size_of::<DirEntry>();
            if offset > cache.block_size() {
                offset = 0;
                block_addr.0 += 1;
            }
            Some((Ok(de), (cache, block_addr, offset)))
        },
    )
}

pub struct LongDirEntry {
    pub entry: DirEntry,
    pub name: SmallVec<[u16; 8]>,
}

impl Deref for LongDirEntry {
    type Target = DirEntry;

    fn deref(&self) -> &Self::Target {
        &self.entry
    }
}

impl LongDirEntry {
    /// Get the long name for this entry as a UTF-16 encoded string.
    pub fn long_name(&self) -> Result<&Utf16Str, widestring::error::Utf16Error> {
        Utf16Str::from_slice(&self.name)
    }
}

pub fn long_dir_entry_stream(
    cache: &mut BlockCache,
    dir_start_address: BlockAddress,
) -> impl Stream<Item = Result<LongDirEntry, Error>> + '_ {
    futures::stream::unfold(
        (cache, dir_start_address, 0),
        move |(cache, mut block_addr, mut offset)| async move {
            let mut entry_bytes = [0u8; 32];
            let mut entry_name = SmallVec::new();

            loop {
                match cache
                    .copy_bytes(block_addr, offset, &mut entry_bytes)
                    .await
                    .context(StorageSnafu)
                {
                    Ok(_) => {}
                    Err(e) => return Some((Err(e), (cache, block_addr, offset))),
                }

                offset += 32;
                if offset > cache.block_size() {
                    offset = 0;
                    block_addr.0 += 1;
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
                        (cache, block_addr, offset),
                    ));
                }
            }
        },
    )
}

pub struct LongDirEntryX<'e> {
    pub entry: &'e DirEntry,
    pub name: SmallVec<[u8; 16]>,
}

impl<'e> Deref for LongDirEntryX<'e> {
    type Target = DirEntry;

    fn deref(&self) -> &Self::Target {
        self.entry
    }
}

impl<'e> LongDirEntryX<'e> {
    /// Get the long name for this entry as a UTF-16 encoded string.
    pub fn long_name(&self) -> Result<&Utf16Str, widestring::error::Utf16Error> {
        // SAFTEY: we have already checked to make sure that the SmallVec<u8> has enough bytes to
        // be sliced into as a u16. This assumes that the system has matching endianness as the volume.
        unsafe {
            Utf16Str::from_slice(core::slice::from_raw_parts(
                self.name.as_ptr() as *const u16,
                self.name.len() / 2,
            ))
        }
    }
}

pub struct DirEntryIter<U> {
    underlying: U,
}

impl<'e, U: Iterator<Item = &'e DirEntry>> From<U> for DirEntryIter<U> {
    fn from(value: U) -> Self {
        DirEntryIter { underlying: value }
    }
}

impl<'e, U: Iterator<Item = &'e DirEntry>> Iterator for DirEntryIter<U> {
    type Item = Result<LongDirEntryX<'e>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut e = self.underlying.next()?;

        if e.short_name[0] == 0x00 {
            return None;
        }

        let mut name = SmallVec::new();
        while e.attributes == DirEntryAttributes::LongName {
            // log::trace!("ld {e:?}");
            let bytes = bytes_of(e);
            name.insert_many(
                0,
                (1..11)
                    .chain(14..26)
                    .chain(28..32)
                    .map(|i| bytes[i])
                    .take_while(|b| *b != 0xff),
            );
            if let Some(ne) = self.underlying.next() {
                e = ne;
            } else {
                return Some(Err(BadMetadataSnafu {
                    message: "unexpected end of directory",
                    value: e.cluster_num_lo,
                }
                .build()));
            }
        }

        if name.len() % 2 == 1 {
            // we include the null byte at the end when we gather the bytes, but if we get here we
            // just need to take that off
            if name.last().is_some_and(|v| *v == 0) {
                name.pop();
            } else {
                return Some(Err(Error::BadMetadata {
                    message: "long name is invalid UTF-16 (odd number of bytes)",
                    value: name.len(),
                }));
            }
        }

        // log::trace!("E {e:?}");
        Some(Ok(LongDirEntryX { entry: e, name }))
    }
}
