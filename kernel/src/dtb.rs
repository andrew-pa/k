use byteorder::{BigEndian, ByteOrder};

const EXPECTED_MAGIC: u32 = 0xd00d_feed;

const FDT_BEGIN_NODE: u8 = 0x01;
const FDT_END_NODE  : u8 = 0x02;
const FDT_PROP      : u8 = 0x03;
const FDT_NOP       : u8 = 0x04;
const FDT_END       : u8 = 0x09;

struct BlobHeader<'a> {
    buf: &'a [u8]
}

impl<'a> BlobHeader<'a> {
    fn magic(&self) -> u32 { BigEndian::read_u32(&self.buf[0..]) }
    fn total_size(&self) -> u32 { BigEndian::read_u32(&self.buf[4..]) }
    fn off_dt_struct(&self) -> u32 { BigEndian::read_u32(&self.buf[8..]) }
    fn off_dt_strings(&self) -> u32 { BigEndian::read_u32(&self.buf[12..]) }
    fn off_mem_rsvmap(&self) -> u32 { BigEndian::read_u32(&self.buf[16..]) }
    fn version(&self) -> u32 { BigEndian::read_u32(&self.buf[20..]) }
    fn last_comp_version(&self) -> u32 { BigEndian::read_u32(&self.buf[24..]) }
    fn boot_cpuid_phys(&self) -> u32 { BigEndian::read_u32(&self.buf[28..]) }
    fn size_dt_strings(&self) -> u32 { BigEndian::read_u32(&self.buf[32..]) }
    fn size_dt_structs(&self) -> u32 { BigEndian::read_u32(&self.buf[36..]) }
}

pub struct DeviceTree {
    buf: &'static [u8],
    strings: &'static [u8],
    structure: &'static [u8]
}

pub enum StructureItem<'dt> {
    StartNode(&'dt str),
    EndNode,
    Property {
        name: &'dt str,
        data: &'dt [u8]
    },
}

pub struct Cursor<'dt> {
    dt: &'dt DeviceTree,
    current_offset: usize
}

impl DeviceTree {
    pub unsafe fn at_address(addr: *mut u8) -> DeviceTree {
        let header = BlobHeader { buf: core::slice::from_raw_parts(addr, 64) };
        if header.magic() != EXPECTED_MAGIC {
            // panic!("unexpected magic value for {addr}, got {}", header.magic())
        }
        let buf = core::slice::from_raw_parts(addr, header.total_size() as usize);
        let header = BlobHeader { buf };
        DeviceTree {
            strings: core::slice::from_raw_parts(addr.offset(header.off_dt_strings() as isize), header.size_dt_strings() as usize),
            structure: core::slice::from_raw_parts(addr.offset(header.off_dt_struct() as isize), header.size_dt_structs() as usize),
            buf,
        }
    }

    pub fn iter(&self) -> Cursor {
        Cursor { current_offset: 0, dt: self }
    }
}

fn pad_end_4b(num_bytes: usize) -> usize {
    num_bytes + if num_bytes % 4 == 0 { 0 } else { 4 - (num_bytes % 4) }
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
                    let name = core::str::from_utf8(&self.dt.structure[self.current_offset..name_end])
                        .expect("device tree node name is utf8");
                    self.current_offset += pad_end_4b(name_end + 1);
                    return Some(StructureItem::StartNode(name))
                },
                2 => return Some(StructureItem::EndNode),
                3 => {
                    let length = BigEndian::read_u32(&self.dt.structure[self.current_offset..]) as usize;
                    self.current_offset += 4;
                    let name_offset = BigEndian::read_u32(&self.dt.structure[self.current_offset..]) as usize;
                    self.current_offset += 4;
                    let mut name_end = name_offset;
                    while self.dt.strings.get(name_end).map_or(false, |b| *b != 0) {
                        name_end += 1;
                    }
                    let name = core::str::from_utf8(&self.dt.strings[name_offset..name_end])
                        .expect("device tree node name is utf8");
                    let data = &self.dt.structure[self.current_offset..(self.current_offset+length)];
                    self.current_offset += pad_end_4b(length);
                    return Some(StructureItem::Property { name, data })
                },
                4 => continue,
                9 => return None,
                _ => panic!()
            }
        }
    }
}
