use core::cell::OnceCell;

use alloc::collections::LinkedList;
use snafu::ensure;
use spin::Mutex;

use super::{MemoryError, VirtualAddress, PAGE_SIZE};

const START_ADDRESS: VirtualAddress = VirtualAddress(0xffff_0010_0000_0000);
const TOTAL_SIZE: usize = 0x0100_0000_0000 / PAGE_SIZE; //1TiB in pages

#[derive(Debug)]
struct FreeBlock {
    address: VirtualAddress,
    size: usize,
}

/// VirtualAddressAllocator allocates virtual address ranges for mapping already allocated memory
/// This is important if the physical address of a piece of memory is neccessary and/or a device has specific physical requirements
/// This allocator requires that the kernel heap is already active
pub struct VirtualAddressAllocator {
    free_list: LinkedList<FreeBlock>,
}

impl VirtualAddressAllocator {
    /// Create a new allocator for a given range of virtual addresses.
    pub fn new(
        start_address: VirtualAddress,
        total_size_in_pages: usize,
    ) -> VirtualAddressAllocator {
        VirtualAddressAllocator {
            free_list: LinkedList::from([FreeBlock {
                address: start_address,
                size: total_size_in_pages,
            }]),
        }
    }

    /// Remove a specific range of virtual addresses from the free pool. These addresses will not be
    /// returned from `alloc()`. To return the addresses to the pool, call `free()` as normal. If
    /// the range cannot be reserved (because it has already been allocated), an error will be
    /// returned. It is best to use this function before ever calling `alloc()`.
    pub fn reserve(
        &mut self,
        start_address: VirtualAddress,
        page_count: usize,
    ) -> Result<(), MemoryError> {
        // find the block that contains the starting address.
        // if the block is too small to contain the entire reserved range, return InsufficentForAllocationSnafu
        // otherwise, remove the reserved range from the block, potentially yielding up to two new
        // free blocks if the range is in the middle

        let end_address = start_address.offset_fwd(page_count * PAGE_SIZE);
        let mut cur = self.free_list.cursor_front_mut();
        while let Some(block) = cur.current() {
            let block_end = block.address.offset_fwd(block.size * PAGE_SIZE);

            if block.address <= start_address && block_end >= end_address {
                // we found the overlapping free block, modify it to accommodate the reserved range
                if block.address == start_address {
                    if block.size == page_count {
                        cur.remove_current();
                    } else {
                        block.address = block.address.offset_fwd(page_count);
                        block.size -= page_count;
                    }
                } else if block_end == end_address {
                    block.size -= page_count;
                } else {
                    block.size = (start_address.0 - block.address.0).div_ceil(PAGE_SIZE);
                    cur.insert_after(FreeBlock {
                        address: end_address,
                        size: (block_end.0 - end_address.0).div_ceil(PAGE_SIZE),
                    })
                }
                return Ok(());
            }

            // stop early if we are definitely past the point where the block would be since the
            // list is in sorted order by address
            if block.address >= end_address {
                break;
            }

            cur.move_next();
        }

        Err(MemoryError::InsufficentForAllocation { size: page_count })
    }

    /// Allocate a range of virtual addresses from the free pool, returning their starting address.
    pub fn alloc(&mut self, page_count: usize) -> Result<VirtualAddress, MemoryError> {
        let mut cur = self.free_list.cursor_front_mut();
        while let Some(block) = cur.current() {
            if block.size >= page_count {
                let addr = block.address;
                if block.size == page_count {
                    cur.remove_current();
                } else {
                    block.address = block.address.offset((PAGE_SIZE * page_count) as isize);
                    block.size -= page_count;
                }
                log::trace!("allocated {page_count} pages at {addr}");
                return Ok(addr);
            }
            cur.move_next();
        }
        Err(MemoryError::OutOfMemory)
    }

    /// Return a range of previously allocated addresses to the pool.
    /// Warning: if you don't free exactly as many pages as you allocated, this will leak any left-over pages
    pub fn free(&mut self, address: VirtualAddress, page_count: usize) {
        log::trace!("freeing {page_count} pages at {address}");
        let end_address = address.offset((PAGE_SIZE * page_count) as isize);
        let mut cur = self.free_list.cursor_front_mut();
        while let Some(block) = cur.current() {
            let back = block.address.offset((PAGE_SIZE * block.size) as isize);
            if end_address == block.address {
                // the block we're freeing is immediately before this block
                block.address = address;
                block.size += page_count;
            } else if address == back {
                // the block we're freeing is immediately after this block
                block.size += page_count;
            } else if block.address > address {
                // insert new free block to maintain sorted order
                cur.insert_before(FreeBlock {
                    address,
                    size: page_count,
                });
                return;
            }
            cur.move_next();
        }
        self.free_list.push_back(FreeBlock {
            address,
            size: page_count,
        });
    }
}

static mut VAA: OnceCell<Mutex<VirtualAddressAllocator>> = OnceCell::new();

pub fn init_virtual_address_allocator() {
    unsafe {
        VAA.set(Mutex::new(VirtualAddressAllocator::new(
            START_ADDRESS,
            TOTAL_SIZE,
        )))
        .ok()
        .expect("init virtual address allocator once");
    }
}

pub fn virtual_address_allocator() -> spin::MutexGuard<'static, VirtualAddressAllocator> {
    unsafe {
        VAA.get()
            .as_ref()
            .expect("virtual address allocator initialized")
            .lock()
    }
}
