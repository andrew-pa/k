use core::cell::OnceCell;

use alloc::collections::LinkedList;
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

    /// Warning: if you don't free exactly as many pages as you allocated, this will leak any left-over pages
    pub fn free(&mut self, address: VirtualAddress, page_count: usize) {
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
        }
        self.free_list.push_back(FreeBlock {
            address,
            size: page_count,
        });
    }
}

static mut VAA: OnceCell<Mutex<VirtualAddressAllocator>> = OnceCell::new();

pub unsafe fn init_virtual_address_allocator() {
    VAA.set(Mutex::new(VirtualAddressAllocator {
        free_list: LinkedList::from([FreeBlock {
            address: START_ADDRESS,
            size: TOTAL_SIZE,
        }]),
    }))
    .ok()
    .expect("init virtual address allocator once");
}

pub fn virtual_address_allocator() -> spin::MutexGuard<'static, VirtualAddressAllocator> {
    unsafe {
        VAA.get()
            .as_ref()
            .expect("virtual address allocator initialized")
            .lock()
    }
}
