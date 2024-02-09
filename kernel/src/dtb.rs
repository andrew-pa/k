//! Device Tree blob parser.
//!
//! This is designed to require no allocation/copying so that it can be used as soon as possible
//! during the boot process.
//! Individual device drivers are expected to make sense of the exact structure of the information
//! in their respective portion of the tree, but this module contains common structures and
//! iterators to make that easier.
use core::{ffi::CStr, fmt::Debug};

use byteorder::{BigEndian, ByteOrder};

use crate::memory::VirtualAddress;

pub const EXPECTED_MAGIC: u32 = 0xd00d_feed;

const FDT_BEGIN_NODE: u8 = 0x01;
const FDT_END_NODE: u8 = 0x02;
const FDT_PROP: u8 = 0x03;
const FDT_NOP: u8 = 0x04;
const FDT_END: u8 = 0x09;

/// Device tree blob header.
struct BlobHeader<'a> {
    buf: &'a [u8],
}

impl<'a> BlobHeader<'a> {
    /// Magic number. Should equal [EXPECTED_MAGIC].
    pub fn magic(&self) -> u32 {
        BigEndian::read_u32(&self.buf[0..])
    }
    /// Total size of the blob.
    pub fn total_size(&self) -> u32 {
        BigEndian::read_u32(&self.buf[4..])
    }
    /// Offset to the structs region of the blob.
    pub fn off_dt_struct(&self) -> u32 {
        BigEndian::read_u32(&self.buf[8..])
    }
    /// Offset to the strings region of the blob.
    pub fn off_dt_strings(&self) -> u32 {
        BigEndian::read_u32(&self.buf[12..])
    }
    /// Offset to the memory reservation block.
    pub fn off_mem_rsvmap(&self) -> u32 {
        BigEndian::read_u32(&self.buf[16..])
    }
    /// Blob version code.
    pub fn version(&self) -> u32 {
        BigEndian::read_u32(&self.buf[20..])
    }
    /// Last compatible version this device tree is compatible with.
    pub fn last_comp_version(&self) -> u32 {
        BigEndian::read_u32(&self.buf[24..])
    }
    /// Physical ID of the boot CPU.
    pub fn boot_cpuid_phys(&self) -> u32 {
        BigEndian::read_u32(&self.buf[28..])
    }
    /// Size of the strings region of the blob.
    pub fn size_dt_strings(&self) -> u32 {
        BigEndian::read_u32(&self.buf[32..])
    }
    /// Size of the structs region of the blob.
    pub fn size_dt_structs(&self) -> u32 {
        BigEndian::read_u32(&self.buf[36..])
    }
}

impl<'a> Debug for BlobHeader<'a> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BlobHeader")
            .field("magic", &self.magic())
            .field("total_size", &self.total_size())
            .field("off_dt_struct", &self.off_dt_struct())
            .field("off_dt_strings", &self.off_dt_strings())
            .field("off_mem_rsvmap", &self.off_mem_rsvmap())
            .field("version", &self.version())
            .field("last_comp_version", &self.last_comp_version())
            .field("boot_cpuid_phys", &self.boot_cpuid_phys())
            .field("size_dt_strings", &self.size_dt_strings())
            .field("size_dt_structs", &self.size_dt_structs())
            .finish()
    }
}

/// A device tree blob in memory.
pub struct DeviceTree<'a> {
    buf: &'a [u8],
    strings: &'a [u8],
    structure: &'a [u8],
    mem_map: &'a [u8],
}

/// Standard forms of device tree property data blobs.
#[derive(Debug)]
pub enum StandardProperty<'dt> {
    /// A list of names of devices compatible with this device.
    Compatible(StringList<'dt>),
    /// Number of cells (u32s) used to encode addresses in this node's `reg` property.
    AddressCells(u32),
    /// Number of cells (u32s) used to encode sizes in this node's `reg` property.
    SizeCells(u32),
}

/// A tree structural item.
#[derive(Debug)]
pub enum StructureItem<'dt> {
    /// The beginning of a node in the tree, with a particular name.
    StartNode(&'dt str),
    /// The end of a node in the tree.
    EndNode,
    /// A property attached to some node.
    Property {
        /// The name of the property.
        name: &'dt str,
        /// The data associated with this property.
        data: &'dt [u8],
        /// If the property has a standard form, the standard interpretation of this property.
        std_interp: Option<StandardProperty<'dt>>,
    },
}

/// A cursor through the device tree.
pub struct Cursor<'dt> {
    dt: &'dt DeviceTree<'dt>,
    current_offset: usize,
}

/// An iterator over reserved regions of memory.
pub struct MemRegionIter<'dt> {
    data: &'dt [u8],
    current_offset: usize,
}

/// A list of strings in the blob.
#[derive(Clone)]
pub struct StringList<'dt> {
    data: &'dt [u8],
    current_offset: usize,
}

impl DeviceTree<'_> {
    /// Create a [`DeviceTree`] struct that represents a device tree blob at some virtual address.
    ///
    /// # Safety
    /// It is up to the caller to make sure that `addr` actually points to a valid, mapped device
    /// tree blob, and that it will live for the `'a` lifetime at this address.
    pub unsafe fn at_address<'a>(addr: VirtualAddress) -> DeviceTree<'a> {
        let addr = addr.as_ptr();
        let header = BlobHeader {
            buf: core::slice::from_raw_parts(addr, 64),
        };
        if header.magic() != EXPECTED_MAGIC {
            panic!(
                "unexpected magic value for {addr:x?}, got {}",
                header.magic()
            )
        }
        let buf = core::slice::from_raw_parts(addr, header.total_size() as usize);
        let header = BlobHeader { buf };
        log::debug!("device tree at {:x}, header={:?}", addr as usize, header);
        DeviceTree {
            strings: core::slice::from_raw_parts(
                addr.offset(header.off_dt_strings() as isize),
                header.size_dt_strings() as usize,
            ),
            structure: core::slice::from_raw_parts(
                addr.offset(header.off_dt_struct() as isize),
                header.size_dt_structs() as usize,
            ),
            mem_map: core::slice::from_raw_parts(
                addr.offset(header.off_mem_rsvmap() as isize),
                header.size_dt_structs() as usize,
            ),
            buf,
        }
    }

    /// Get the header for the blob.
    fn header(&self) -> BlobHeader {
        BlobHeader { buf: self.buf }
    }

    /// Returns the total size of the blob in bytes.
    pub fn size_of_blob(&self) -> usize {
        self.header().total_size() as usize
    }

    /// Iterate over the tree structure.
    pub fn iter_structure(&self) -> Cursor {
        Cursor {
            current_offset: 0,
            dt: self,
        }
    }

    /// Finds the node named `node_name` in the tree and then calls `f` for each of its properties,
    /// ignoring any of its children. Returns true if the node was found.
    ///
    /// `f` recieves the name of the property, the raw data bytes associated, and any standard
    /// interpretation of that data.
    pub fn process_properties_for_node<'s>(
        &'s self,
        node_name: &str,
        mut f: impl FnMut(&'s str, &'s [u8], Option<StandardProperty<'s>>),
    ) -> bool {
        let mut found_node = false;
        let mut dt = self.iter_structure();
        while let Some(n) = dt.next() {
            match n {
                StructureItem::StartNode(name) if name.starts_with(node_name) => {
                    found_node = true;
                }
                StructureItem::StartNode(_) if found_node => {
                    for j in dt.by_ref() {
                        if let StructureItem::EndNode = j {
                            break;
                        }
                    }
                }
                StructureItem::EndNode if found_node => break,
                StructureItem::Property {
                    name,
                    data,
                    std_interp,
                } if found_node => f(name, data, std_interp),
                _ => {}
            }
        }
        found_node
    }

    /// Iterate over the system reserved memory regions.
    pub fn iter_reserved_memory_regions(&self) -> MemRegionIter {
        MemRegionIter::for_data(self.mem_map)
    }

    /// Write the device tree to the system log at DEBUG level.
    pub fn log(&self) {
        log::debug!("Device tree:");
        for item in self.iter_structure() {
            log::debug!("{item:x?}");
        }
        log::debug!("-----------");
    }
}

fn pad_end_4b(num_bytes: usize) -> usize {
    num_bytes
        + if num_bytes % 4 == 0 {
            0
        } else {
            4 - (num_bytes % 4)
        }
}

impl<'dt> Iterator for Cursor<'dt> {
    type Item = StructureItem<'dt>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            self.current_offset += 4;
            match BigEndian::read_u32(&self.dt.structure[(self.current_offset - 4)..]) {
                1 => {
                    let mut name_end = self.current_offset;
                    while self.dt.structure.get(name_end).map_or(false, |b| *b != 0) {
                        name_end += 1;
                    }
                    let name =
                        core::str::from_utf8(&self.dt.structure[self.current_offset..name_end])
                            .expect("device tree node name is utf8");
                    self.current_offset = pad_end_4b(name_end + 1);
                    return Some(StructureItem::StartNode(name));
                }
                2 => return Some(StructureItem::EndNode),
                3 => {
                    let length =
                        BigEndian::read_u32(&self.dt.structure[self.current_offset..]) as usize;
                    self.current_offset += 4;
                    let name_offset =
                        BigEndian::read_u32(&self.dt.structure[self.current_offset..]) as usize;
                    self.current_offset += 4;
                    let mut name_end = name_offset;
                    while self.dt.strings.get(name_end).map_or(false, |b| *b != 0) {
                        name_end += 1;
                    }
                    let name = core::str::from_utf8(&self.dt.strings[name_offset..name_end])
                        .expect("device tree node name is utf8");
                    let data =
                        &self.dt.structure[self.current_offset..(self.current_offset + length)];
                    self.current_offset += pad_end_4b(length);
                    let std_interp = match name {
                        "#address-cells" => {
                            Some(StandardProperty::AddressCells(BigEndian::read_u32(data)))
                        }
                        "#size-cells" => {
                            Some(StandardProperty::SizeCells(BigEndian::read_u32(data)))
                        }
                        "compatible" => Some(StandardProperty::Compatible(StringList {
                            data,
                            current_offset: 0,
                        })),
                        _ => None,
                    };
                    return Some(StructureItem::Property {
                        name,
                        data,
                        std_interp,
                    });
                }
                4 => continue,
                9 => return None,
                x => panic!("unknown device tree token: {x}"),
            }
        }
    }
}

impl<'dt> MemRegionIter<'dt> {
    /// Creates a memory region iterator for the data of an arbitrary property.
    pub fn for_data(data: &'dt [u8]) -> Self {
        Self {
            data,
            current_offset: 0,
        }
    }
}

impl<'dt> Iterator for MemRegionIter<'dt> {
    type Item = (u64, u64);

    fn next(&mut self) -> Option<Self::Item> {
        let addr = BigEndian::read_u64(&self.data[self.current_offset..]);
        self.current_offset += 8;
        let size = BigEndian::read_u64(&self.data[self.current_offset..]);
        self.current_offset += 8;
        if addr == 0 && size == 0 {
            None
        } else {
            Some((addr, size))
        }
    }
}

impl<'dt> Iterator for StringList<'dt> {
    type Item = &'dt CStr;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_offset >= self.data.len() {
            None
        } else {
            match CStr::from_bytes_until_nul(&self.data[self.current_offset..]) {
                Ok(s) => {
                    self.current_offset += s.to_bytes_with_nul().len();
                    Some(s)
                }
                Err(e) => None,
            }
        }
    }
}

impl<'dt> core::fmt::Debug for StringList<'dt> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_list().entries(self.clone()).finish()
    }
}
