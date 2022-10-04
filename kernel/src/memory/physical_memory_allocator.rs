use crate::{dtb::{StructureItem, DeviceTree}, memory::{PAGE_SIZE, __kernel_end}};
use byteorder::{ByteOrder, BigEndian};

use super::PhysicalAddress;

struct StaticBitmap {
    bitmap: &'static mut [u64]
}

impl StaticBitmap {
    unsafe fn new(addr: *mut u64, size: usize) -> StaticBitmap {
        StaticBitmap { bitmap: core::slice::from_raw_parts_mut(addr, size/64) }
    }

    fn get(&self, index: usize) -> Option<bool> {
        self.bitmap.get(index / 64)
            .map(|word| {
                (word >> (index % 64)) & 1 == 1
            })
    }

    fn toggle(&mut self, index: usize) {
        self.bitmap.get_mut(index / 64)
            .map(|word| {
                *word = *word ^ (1 << (index % 64));
            });
    }

    fn len(&self) -> usize { self.bitmap.len() * 64 }
}

pub struct PhysicalMemoryAllocator {
    allocated_pages: StaticBitmap
}

impl PhysicalMemoryAllocator {
    pub fn init(device_tree: &DeviceTree) -> PhysicalMemoryAllocator {
        // for now, find the first memory node and use it to determine how big RAM is
        let memory_props = device_tree.iter_structure().skip_while(|i| match i {
            StructureItem::StartNode(name) if name.starts_with("memory") => false,
            _ => true
        }).find_map(|i| match i {
            StructureItem::Property { name, data } if name == "reg" => Some(data),
            _ => None
        }).expect("RAM properties in device tree");
        let mem_start = BigEndian::read_u64(&memory_props);
        let mem_length = BigEndian::read_u64(&memory_props[8..]) as usize;
        log::info!("RAM starts at 0x{mem_start:x} and is 0x{mem_length:x} bytes long");

        // TODO: find reserved memory regions and do something with them
        for (addr, size) in device_tree.iter_reserved_memory_regions() {
            log::info!("reserved memory region at 0x{addr:x}, size={size}");
        }

        PhysicalMemoryAllocator {
            allocated_pages: unsafe {
                StaticBitmap::new(&mut __kernel_end as *mut u64, mem_length / PAGE_SIZE)
            }
        }
    }

    pub fn alloc(&mut self) -> PhysicalAddress {
        self.alloc_contig(1)
    }

    pub fn alloc_contig(&mut self, pages: usize) -> PhysicalAddress {
    }
}
