//! The physical memory allocator.
use core::cell::OnceCell;

use crate::{
    ds::dtb::{DeviceTree, StructureItem},
    memory::{__kernel_end, __kernel_start, PAGE_SIZE},
};
use bitvec::{index::BitIdx, prelude::*};
use byteorder::{BigEndian, ByteOrder};
use spin::Mutex;

use super::{MemoryError, PhysicalAddress};

/// The allocator for physical pages of memory.
///
/// This allocator is currently a bitmap-style allocator.
///
/// There is a single instance of this allocator responsible for managing all the pages of physical
/// memory in the system, accessable through [physical_memory_allocator].
pub struct PhysicalMemoryAllocator {
    allocated_pages: &'static mut BitSlice,
    memory_start: usize,
    memory_length: usize,
}

impl PhysicalMemoryAllocator {
    /// Initalize the allocator using information in the device tree.
    fn init(device_tree: &DeviceTree) -> PhysicalMemoryAllocator {
        // for now, find the first memory node and use it to determine how big RAM is
        let memory_props = device_tree
            .iter_structure()
            .skip_while(
                |i| !matches!(i, StructureItem::StartNode(name) if name.starts_with("memory")),
            )
            .find_map(|i| match i {
                StructureItem::Property {
                    name: "reg", data, ..
                } => Some(data),
                _ => None,
            })
            .expect("RAM properties in device tree");
        let memory_start = BigEndian::read_u64(memory_props) as usize;
        let memory_length = BigEndian::read_u64(&memory_props[8..]) as usize;
        log::info!("RAM starts at 0x{memory_start:x} and is 0x{memory_length:x} bytes long");

        // TODO: find reserved memory regions and do something with them?
        //  Right now we just exclude anything above RAM from allocation, but it could be possible
        //  to have reserved pages below RAM that need to be pre-allocated
        for (addr, size) in device_tree.iter_reserved_memory_regions() {
            log::info!("reserved memory region at 0x{addr:x}, size={size}");
        }

        // these addresses will be in high memory, so they need shifted to be physical addresses
        let kernel_start =
            unsafe { core::ptr::addr_of!(__kernel_start) as usize } - 0xffff_0000_0000_0000;
        let kernel_end =
            unsafe { core::ptr::addr_of!(__kernel_end) as usize } - 0xffff_0000_0000_0000;
        log::info!("kernel_start = p:0x{kernel_start:x}, kernel_end = p:0x{kernel_end:x}");

        // assumes 64bit alignment/8bpb
        let padding = if kernel_end % 8 == 0 {
            0
        } else {
            8 - kernel_end % 8
        };

        // make sure the bitmap starts on a word boundary and is placed in virtual memory correctly
        let alloc_bitmap_addr = (0xffff_0000_0000_0000 + kernel_end + padding) as *mut usize;

        let allocated_pages = unsafe {
            bitvec::slice::from_raw_parts_unchecked_mut(
                BitPtr::new((&mut *alloc_bitmap_addr).into(), BitIdx::new(0).unwrap()).unwrap(),
                memory_length / PAGE_SIZE,
            )
        };

        allocated_pages.fill(false);

        // allocate the space taken up by the device tree blob
        let dtb_len_pages = device_tree.size_of_blob() / PAGE_SIZE;
        log::debug!("device tree takes {dtb_len_pages} pages");
        allocated_pages[0..dtb_len_pages].fill(true);
        // allocate the space taken up by the kernel image
        let kernel_start_pages = (kernel_start - memory_start).div_ceil(PAGE_SIZE);
        let kernel_end_pages = (kernel_end - memory_start).div_ceil(PAGE_SIZE);
        log::debug!(
            "kernel image takes {} pages",
            kernel_end_pages - kernel_start_pages
        );
        allocated_pages[kernel_start_pages..kernel_end_pages].fill(true);
        // allocate the space taken up by the page allocation bitmap
        allocated_pages[kernel_end_pages
            ..(kernel_end_pages + (memory_length / PAGE_SIZE / 8 + padding).div_ceil(PAGE_SIZE))]
            .fill(true);

        PhysicalMemoryAllocator {
            allocated_pages,
            memory_start,
            memory_length,
        }
    }

    /// Get the physical address of the start of main memory (RAM).
    pub fn memory_start_addr(&self) -> PhysicalAddress {
        PhysicalAddress(self.memory_start)
    }

    /// Get the total size of main memory (RAM) in bytes.
    pub fn total_memory_size(&self) -> usize {
        self.memory_length
    }

    /// Allocate a single physical page.
    pub fn alloc(&mut self) -> Result<PhysicalAddress, MemoryError> {
        self.alloc_contig(1)
    }

    /// Free a physical page starting at `base_address`.
    pub fn free(&mut self, base_address: PhysicalAddress) {
        self.free_pages(base_address, 1)
    }

    /// Free a contiguous range of pages of length `page_count` starting at `base_address`.
    pub fn free_pages(&mut self, base_address: PhysicalAddress, page_count: usize) {
        let page_index = (base_address.0 - self.memory_start).div_ceil(PAGE_SIZE);
        if self.allocated_pages[page_index..(page_index + page_count)].not_all() {
            log::warn!("double free at {}, {} pages", base_address, page_count);
        }
        self.allocated_pages[page_index..(page_index + page_count)].fill(false);
        log::trace!("freed {page_count} pages at {base_address}");
    }

    /// Allocate a contiguous range of pages of length `page_count`.
    ///
    /// It is best to avoid allocating large ranges if possible, to avoid fragmenting physical
    /// memory.
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
                        }
                        start_index = Some(i);
                        break;
                    }
                    None => return Err(MemoryError::OutOfMemory),
                }
            }

            // check to see if there are enough pages here
            for _ in 0..(page_count - 1) {
                match pi.next() {
                    Some((_, allocated)) => {
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

    /// Try to allocate a contiguous range of pages of length `page_count`. If a smaller contiguous
    /// range is found first, it will be returned. The return on success contains the address and
    /// number of pages allocated.
    pub fn try_alloc_contig(
        &mut self,
        desired_page_count: usize,
    ) -> Result<(PhysicalAddress, usize), MemoryError> {
        let mut pi = self.allocated_pages.iter_mut().enumerate();
        // skip until an empty page
        let start_index;
        loop {
            match pi.next() {
                Some((i, allocated)) => {
                    if *allocated {
                        continue;
                    }
                    start_index = Some(i);
                    break;
                }
                None => return Err(MemoryError::OutOfMemory),
            }
        }

        let mut actual_page_count = desired_page_count;
        // check to see if there are enough pages here
        for i in 0..(desired_page_count - 1) {
            match pi.next() {
                Some((_, allocated)) => {
                    if *allocated {
                        actual_page_count = i + 1;
                        break;
                    }
                }
                None => {
                    return Err(MemoryError::InsufficentForAllocation {
                        size: desired_page_count * PAGE_SIZE,
                    })
                }
            }
        }

        // we found enough pages, mark them as allocated and return
        let start_index = start_index.unwrap();
        self.allocated_pages[start_index..(start_index + actual_page_count)].fill(true);
        let addr = PhysicalAddress(self.memory_start + start_index * PAGE_SIZE);
        log::trace!("allocated {actual_page_count} pages at {addr}");
        Ok((addr, actual_page_count))
    }
}

static mut PMA: OnceCell<Mutex<PhysicalMemoryAllocator>> = OnceCell::new();

/// Initalize the physical memory allocator, using information from the device tree.
pub fn init_physical_memory_allocator(device_tree: &DeviceTree) {
    unsafe {
        PMA.set(Mutex::new(PhysicalMemoryAllocator::init(device_tree)))
            .ok()
            .expect("init physical memory once");
    }
}

/// Lock and gain access to the current physical memory allocator.
pub fn physical_memory_allocator() -> spin::MutexGuard<'static, PhysicalMemoryAllocator> {
    unsafe {
        PMA.get()
            .as_ref()
            .expect("physical memory allocator initalized")
            .lock()
    }
}
