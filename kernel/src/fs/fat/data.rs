//! Data structures for FAT filesystems
use core::ops::Deref;

use alloc::string::String;
use bitflags::bitflags;
use bytemuck::{bytes_of, bytes_of_mut, from_bytes, Pod, Zeroable};
use futures::Stream;
use smallvec::SmallVec;
use snafu::Snafu;
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
            self.bytes_per_sector as u64 == 512,
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

#[derive(Debug, Snafu)]
pub enum FatError {
    InvalidUtf16 {
        #[snafu(source(false))]
        source: widestring::error::Utf16Error,
        entry: DirEntry,
    },

    InvalidShortName {
        reason: &'static str,
        name: String,
    },
}

impl LongDirEntry {
    /// Get the long name for this entry as a UTF-16 encoded string. If no long name was present, the empty string is returned.
    pub fn long_name(&self) -> Result<&Utf16Str, widestring::error::Utf16Error> {
        Utf16Str::from_slice(&self.name)
    }

    /// Determine if this directory entry's name matches `name`.
    pub fn is_named(&self, name: &str) -> Result<bool, FatError> {
        if self.name.is_empty() {
            if name.len() <= 11 {
                let (name, ext) = name.split_once('.').unwrap_or_else(|| (&name[0..], ""));
                let ename = (&self.short_name[0..8]);
                let eext = &self.short_name[8..11];
                // compare by bytes b/c if the name isn't ASCII then it can't match
                // regardless
                Ok(name
                    .bytes()
                    .eq(ename.iter().copied().take_while(|b| *b != b' '))
                    && ext.bytes().eq(eext.iter().copied()))
            } else {
                Ok(false)
            }
        } else {
            let ln = self.long_name().map_err(|source| {
                InvalidUtf16Snafu {
                    source,
                    entry: self.entry,
                }
                .build()
            })?;
            Ok(ln.chars().eq(name.chars()))
        }
    }
}

pub type ClusterIndex = u16;

#[derive(Debug)]
pub enum DirectorySource {
    Direct(BlockAddress),
    Clusters(ClusterIndex),
}
