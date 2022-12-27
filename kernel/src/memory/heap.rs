use core::{
    alloc::{GlobalAlloc, Layout},
    mem::size_of,
    ptr::{null_mut, NonNull},
    sync::atomic::AtomicUsize,
};

use spin::Mutex;

use super::{VirtualAddress, PAGE_SIZE};

const KERNEL_HEAP_START: VirtualAddress = VirtualAddress(0xffff_8000_0000_0000);
const INIT_SIZE: usize = 8; // pages
const ALLOC_HEADER_SIZE: usize = size_of::<usize>();

#[derive(Debug)]
struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
    prev: *mut FreeBlock,
}

struct KernelGlobalAlloc {
    free_list_head: Mutex<*mut FreeBlock>,
    heap_size: AtomicUsize,
}

unsafe impl Sync for KernelGlobalAlloc {}

#[global_allocator]
static GLOBAL_HEAP: KernelGlobalAlloc = KernelGlobalAlloc {
    free_list_head: Mutex::new(null_mut()),
    heap_size: AtomicUsize::new(0),
};

impl KernelGlobalAlloc {
    fn init(&self) {
        log::info!("initializing kernel heap");
        let mut pt = super::paging::kernel_table();
        let pages = {
            // drop this early to make sure that the page table can also allocate physical memory
            let mut pma = super::physical_memory_allocator();
            pma.alloc_contig(INIT_SIZE)
                .expect("allocate pages for kernel heap")
        };
        log::trace!("initial physical pages at {pages}");
        pt.map_range(pages, KERNEL_HEAP_START, INIT_SIZE, true)
            .expect("map kernel heap pages");
        let block: *mut FreeBlock = KERNEL_HEAP_START.as_ptr();
        log::trace!("initial head block at 0x{:x}", block as usize);
        unsafe {
            let b = block.as_mut().unwrap();
            b.next = null_mut();
            b.prev = null_mut();
            b.size = INIT_SIZE * PAGE_SIZE;
            log::trace!("initial head block = {b:?}");
        }
        *self.free_list_head.lock() = block;
        self.heap_size
            .store(INIT_SIZE, core::sync::atomic::Ordering::Release);
    }

    fn find_suitable_free_block(&self, layout: &Layout) -> Option<NonNull<FreeBlock>> {
        let mut free_list_head = self.free_list_head.lock();
        if free_list_head.is_null() {
            drop(free_list_head);
            self.init();
            free_list_head = self.free_list_head.lock();
        }

        // find a suitable block or null if none are found
        let block = unsafe {
            let mut cur = *free_list_head;
            while let Some(cur_block) = cur.as_ref() {
                if cur_block.size >= layout.size() + ALLOC_HEADER_SIZE {
                    break;
                }
                cur = cur_block.next;
            }
            cur
        };

        if let Some(b) = unsafe { block.as_mut() } {
            // remove the block from the list
            unsafe {
                if let Some(b_prev) = b.prev.as_mut() {
                    b_prev.next = b.next;
                    if let Some(n) = b.next.as_mut() {
                        n.prev = b.prev;
                    }
                } else {
                    *free_list_head = b.next;
                    if let Some(n) = b.next.as_mut() {
                        n.prev = null_mut();
                    }
                }
            }
            NonNull::new(block)
        } else {
            None
        }
    }

    fn add_free_block(&self, location: *mut u8, size: usize) {
        fn check_adj(
            cur_block_loc: usize,
            cur_block_size: usize,
            new_block_loc: usize,
            new_block_size: usize,
        ) -> i8 {
            if new_block_loc == cur_block_loc + cur_block_size {
                1
            } else if cur_block_loc == new_block_loc + new_block_size {
                -1
            } else {
                0 // not adj
            }
        }

        log::trace!(
            "returning block at 0x{:x} of size {}",
            location as usize,
            size
        );
        if location.is_null() {
            log::warn!("attempted to free nullptr of size {size}");
            return;
        }
        let new_block = location as *mut FreeBlock;
        // TODO: return pages back to the PMA?
        /* iterate through free list:
         *     if we've found a block that is adjacent to the block we're trying to free, merge
         *     them together and break
         *     if we've found the spot the block should be inserted to keep the list in sorted
         *     order by address:
         *         check the next block to see if it is adjacent to the current block and merge if so
         *         if not, insert the block here and break
         */
        let mut head = self.free_list_head.lock();
        let mut cur = *head;
        let mut insert_spot = null_mut();
        while let Some(cur_block) = unsafe { cur.as_mut() } {
            match check_adj(cur as usize, cur_block.size, new_block as usize, size) {
                -1 => {
                    // new_block is before cur_block
                    let nb = unsafe { new_block.as_mut().expect("new block non-null") };
                    nb.size = cur_block.size + size;
                    nb.next = cur_block.next;
                    nb.prev = cur_block.prev;
                    if let Some(prev_block) = unsafe { cur_block.prev.as_mut() } {
                        prev_block.next = new_block;
                    }
                    if let Some(next_block) = unsafe { cur_block.next.as_mut() } {
                        next_block.prev = new_block;
                    }
                    return;
                }
                1 => {
                    // new_block is after cur_block
                    cur_block.size += size;
                    return;
                }
                _ => {
                    // blocks are not adjacent
                    if cur as usize > (new_block as usize) {
                        insert_spot = cur;
                        break;
                    }
                }
            }
            insert_spot = cur;
            cur = cur_block.next;
        }

        // it wasn't possible to merge with any of the other blocks
        let nb = unsafe { new_block.as_mut().expect("new block non-null") };
        nb.size = size;
        nb.next = null_mut();
        nb.prev = null_mut();
        match unsafe { insert_spot.as_mut() } {
            // inserting at the end of the list
            Some(s) if s.next.is_null() => {
                log::trace!("inserting new free block at the end of the list {s:?}");
                s.next = new_block;
                nb.prev = insert_spot;
            }
            // inserting in the middle of the list before insert_spot
            Some(s) => {
                log::trace!("inserting new free block in the middle of the list {s:?}");
                nb.next = insert_spot;
                nb.prev = s.prev;
                s.prev = new_block;
                if let Some(prev) = unsafe { s.prev.as_mut() } {
                    prev.next = new_block;
                } else {
                    *head = new_block;
                }
            }
            // there aren't any blocks
            None => {
                log::trace!("inserting new free block at the front of the list");
                assert!(head.is_null());
                *head = new_block;
            }
        }
    }

    fn increase_heap_size(&self, layout: &Layout) {
        let mut pt = super::paging::kernel_table();
        let num_pages = (layout.size() + ALLOC_HEADER_SIZE).div_ceil(PAGE_SIZE);
        let old_heap_size = self
            .heap_size
            .fetch_add(num_pages, core::sync::atomic::Ordering::AcqRel);
        let pages = {
            let mut pma = super::physical_memory_allocator();
            pma.alloc_contig(num_pages)
                .expect("allocate pages for kernel heap")
        };
        let old_heap_end = VirtualAddress(KERNEL_HEAP_START.0 + old_heap_size * PAGE_SIZE);
        pt.map_range(pages, old_heap_end, num_pages, true)
            .expect("map kernel heap pages");
        let block: *mut FreeBlock = old_heap_end.as_ptr();
        let mut head = self.free_list_head.lock();
        unsafe {
            let b = block.as_mut().unwrap();
            b.next = *head;
            b.prev = null_mut();
            b.size = num_pages * PAGE_SIZE;
            head.as_mut().expect("non-null head").prev = block;
        }
        *head = block;
    }

    fn log_heap_info(&self, level: log::Level) {
        let heap_size = self.heap_size.load(core::sync::atomic::Ordering::Acquire);
        log::log!(
            level,
            "total heap size = {} pages ({} bytes)",
            heap_size,
            heap_size * PAGE_SIZE
        );
        let mut free_size = 0;
        unsafe {
            let mut cur = *self.free_list_head.lock();
            while let Some(cur_block) = cur.as_ref() {
                log::log!(level, "0x{:x}: {cur_block:?}", cur as usize);
                free_size += cur_block.size;
                cur = cur_block.next;
            }
        }
        log::log!(
            level,
            "total free = {free_size}b, total allocated = {}b",
            heap_size * PAGE_SIZE - free_size
        );
    }
}

unsafe impl GlobalAlloc for KernelGlobalAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        log::trace!("alloc {layout:?}");
        // find and remove a free block that is >= size
        if let Some(mut block) = self.find_suitable_free_block(&layout) {
            // make it allocated, returning any extra back to the free list
            let p = block.as_ptr() as *mut u8;
            let total_block_size = unsafe { block.as_ref().size };
            log::trace!("found block at {:x} of size {total_block_size}", p as usize);
            let req_block_size = layout.size() + ALLOC_HEADER_SIZE;
            if total_block_size - req_block_size > size_of::<FreeBlock>() {
                // split the block
                self.add_free_block(
                    p.offset(req_block_size as isize),
                    total_block_size - req_block_size,
                );
                unsafe {
                    block.as_mut().size = req_block_size;
                }
            } else {
                // use the whole block
            }
            // return ptr to new allocated block
            p.offset(ALLOC_HEADER_SIZE as isize)
        } else {
            self.increase_heap_size(&layout);
            // try to allocate again
            self.alloc(layout)
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        log::trace!("dealloc 0x{:x} {layout:?}", ptr as usize);
        // TODO: we never return physical memory back to the system once it has been allocated, it just goes back in the heap free pool
        // TODO: we never use the allocated header value but we should probably check it
        self.add_free_block(
            ptr.offset(-(ALLOC_HEADER_SIZE as isize)),
            layout.size() + ALLOC_HEADER_SIZE,
        )
    }
}

pub fn log_heap_info(level: log::Level) {
    GLOBAL_HEAP.log_heap_info(level);
}
