use crate::{
    dtb::{DeviceTree, StructureItem},
    memory::{__kernel_end, PAGE_SIZE, __kernel_start},
};
use bitvec::{index::BitIdx, prelude::*};
use byteorder::{BigEndian, ByteOrder};

use super::{MemoryError, PhysicalAddress};

pub struct PhysicalMemoryAllocator {
    allocated_pages: &'static mut BitSlice,
    memory_start: usize,
    memory_length: usize,
}

impl PhysicalMemoryAllocator {
    pub fn init(device_tree: &DeviceTree) -> PhysicalMemoryAllocator {
        // for now, find the first memory node and use it to determine how big RAM is
        let memory_props = device_tree
            .iter_structure()
            .skip_while(|i| match i {
                StructureItem::StartNode(name) if name.starts_with("memory") => false,
                _ => true,
            })
            .find_map(|i| match i {
                StructureItem::Property { name, data } if name == "reg" => Some(data),
                _ => None,
            })
            .expect("RAM properties in device tree");
        let memory_start = BigEndian::read_u64(&memory_props) as usize;
        let memory_length = BigEndian::read_u64(&memory_props[8..]) as usize;
        log::info!("RAM starts at 0x{memory_start:x} and is 0x{memory_length:x} bytes long");

        // TODO: find reserved memory regions and do something with them?
        //  Right now we just exclude anything above RAM from allocation, but it could be possible
        //  to have reserved pages below RAM that need to be pre-allocated
        for (addr, size) in device_tree.iter_reserved_memory_regions() {
            log::info!("reserved memory region at 0x{addr:x}, size={size}");
        }

        let kernel_start = unsafe { (&__kernel_start as *const u8) as usize };
        let kernel_end = unsafe { (&__kernel_end as *const u8) as usize };
        log::info!("kernel_start = p:0x{kernel_start:x}, kernel_end = p:0x{kernel_end:x}");

        // assumes 64bit alignment/8bpb
        let padding = if kernel_end % 8 == 0 { 0 } else { 8 - kernel_end % 8 };

        // make sure the bitmap starts on a word boundary
        let alloc_bitmap_addr = (kernel_end + padding) as *mut usize;

        let allocated_pages = unsafe {
            bitvec::slice::from_raw_parts_unchecked_mut(
                BitPtr::new((&mut *alloc_bitmap_addr).into(), BitIdx::new(0).unwrap()).unwrap(),
                memory_length / PAGE_SIZE,
            )
        };

        allocated_pages.fill(false);

        // allocate the space taken up by the device tree blob
        let dtb_len_pages = device_tree.header().total_size() as usize / PAGE_SIZE;
        log::debug!("device tree takes {dtb_len_pages} pages");
        allocated_pages[0..dtb_len_pages].fill(true);
        // allocate the space taken up by the kernel image
        let kernel_start_pages = (kernel_start - memory_start).div_ceil(PAGE_SIZE);
        let kernel_end_pages = (kernel_end - memory_start).div_ceil(PAGE_SIZE);
        log::debug!("kernel image takes {} pages", kernel_end_pages - kernel_start_pages);
        allocated_pages[kernel_start_pages..kernel_end_pages].fill(true);
        // allocate the space taken up by the page allocation bitmap
        allocated_pages[kernel_end_pages..(kernel_end_pages + (memory_length/PAGE_SIZE/8 + padding).div_ceil(PAGE_SIZE))].fill(true);

        PhysicalMemoryAllocator {
            allocated_pages,
            memory_start,
            memory_length,
        }
    }

    pub fn alloc(&mut self) -> Result<PhysicalAddress, MemoryError> {
        self.alloc_contig(1)
    }

    pub fn free(&mut self, base_address: PhysicalAddress) {
        self.free_pages(base_address, 1)
    }

    pub fn free_pages(&mut self, base_address: PhysicalAddress, page_count: usize) {
        let page_index = (base_address.0 - self.memory_start).div_ceil(PAGE_SIZE);
        if self.allocated_pages[page_index..(page_index + page_count)].not_all() {
            log::warn!("double free at {}, {} pages", base_address, page_count);
        }
        self.allocated_pages[page_index..(page_index + page_count)].fill(false);
        log::trace!("freed {page_count} pages at {base_address}");
    }

    pub fn alloc_contig(&mut self, page_count: usize) -> Result<PhysicalAddress, MemoryError> {
        let mut pi = self.allocated_pages.iter_mut().enumerate();
        'top: loop {
            // skip until an empty page
            let start_index;
            loop {
                match pi.next() {
                    Some((i, allocated)) => {
                        if *allocated {
                            continue;
                        } else {
                            start_index = Some(i);
                            break;
                        }
                    }
                    None => return Err(MemoryError::OutOfMemory),
                }
            }

            // check to see if there are enough pages here
            for _ in 0..(page_count - 1) {
                match pi.next() {
                    Some((i, allocated)) => {
                        if *allocated {
                            // go find the next range of unallocated pages, this one is too small
                            continue 'top;
                        }
                    }
                    None => {
                        return Err(MemoryError::InsufficentForAllocation {
                            size: page_count * PAGE_SIZE,
                        })
                    }
                }
            }
            
            // we found enough pages, mark them as allocated and return
            let start_index = start_index.unwrap();
            self.allocated_pages[start_index..(start_index + page_count)].fill(true);
            let addr = PhysicalAddress(self.memory_start + start_index * PAGE_SIZE);
            log::trace!("allocated {page_count} pages at {addr}");
            return Ok(addr);
        }
    }
}
