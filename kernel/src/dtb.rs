use core::{ffi::CStr, fmt::Debug};

use byteorder::{BigEndian, ByteOrder};

const EXPECTED_MAGIC: u32 = 0xd00d_feed;

const FDT_BEGIN_NODE: u8 = 0x01;
const FDT_END_NODE: u8 = 0x02;
const FDT_PROP: u8 = 0x03;
const FDT_NOP: u8 = 0x04;
const FDT_END: u8 = 0x09;

pub struct BlobHeader<'a> {
    buf: &'a [u8],
}

impl<'a> BlobHeader<'a> {
    pub fn magic(&self) -> u32 {
        BigEndian::read_u32(&self.buf[0..])
    }
    pub fn total_size(&self) -> u32 {
        BigEndian::read_u32(&self.buf[4..])
    }
    pub fn off_dt_struct(&self) -> u32 {
        BigEndian::read_u32(&self.buf[8..])
    }
    pub fn off_dt_strings(&self) -> u32 {
        BigEndian::read_u32(&self.buf[12..])
    }
    pub fn off_mem_rsvmap(&self) -> u32 {
        BigEndian::read_u32(&self.buf[16..])
    }
    pub fn version(&self) -> u32 {
        BigEndian::read_u32(&self.buf[20..])
    }
    pub fn last_comp_version(&self) -> u32 {
        BigEndian::read_u32(&self.buf[24..])
    }
    pub fn boot_cpuid_phys(&self) -> u32 {
        BigEndian::read_u32(&self.buf[28..])
    }
    pub fn size_dt_strings(&self) -> u32 {
        BigEndian::read_u32(&self.buf[32..])
    }
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

pub struct DeviceTree {
    buf: &'static [u8],
    strings: &'static [u8],
    structure: &'static [u8],
    mem_map: &'static [u8],
}

#[derive(Debug)]
pub enum StandardProperty<'dt> {
    Compatible(StringList<'dt>),
    AddressCells(u32),
    SizeCells(u32),
}

#[derive(Debug)]
pub enum StructureItem<'dt> {
    StartNode(&'dt str),
    EndNode,
    Property {
        name: &'dt str,
        data: &'dt [u8],
        std_interp: Option<StandardProperty<'dt>>,
    },
}

pub struct Cursor<'dt> {
    dt: &'dt DeviceTree,
    current_offset: usize,
}

pub struct MemRegionIter<'dt> {
    data: &'dt [u8],
    current_offset: usize,
}

#[derive(Clone)]
pub struct StringList<'dt> {
    data: &'dt [u8],
    current_offset: usize,
}

impl DeviceTree {
    pub unsafe fn at_address(addr: *mut u8) -> DeviceTree {
        let header = BlobHeader {
            buf: core::slice::from_raw_parts(addr, 64),
        };
        if header.magic() != EXPECTED_MAGIC {
            log::error!(
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

    pub fn header(&self) -> BlobHeader {
        BlobHeader { buf: self.buf }
    }

    pub fn iter_structure(&self) -> Cursor {
        Cursor {
            current_offset: 0,
            dt: self,
        }
    }

    pub fn iter_reserved_memory_regions(&self) -> MemRegionIter {
        MemRegionIter::for_data(self.mem_map)
    }

    pub fn log(&self) {
        log::debug!("Device tree:");
        for item in self.iter_structure() {
            log::debug!("{item:?}");
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
