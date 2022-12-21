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
        log::trace!(
            "returning block at 0x{:x} of size {}",
            location as usize,
            size
        );
        let new_block = location as *mut FreeBlock;
        unsafe {
            let mut head = self.free_list_head.lock();
            let nb = new_block.as_mut().expect("new block non-null");
            nb.size = size;
            nb.prev = null_mut();
            nb.next = *head;
            if let Some(head) = head.as_mut() {
                head.prev = nb;
            }
            *head = new_block;
        }
        // TODO: coalesce blocks? return pages back to the PMA?
    }

    fn increase_heap_size(&self, layout: &Layout) {
        let mut pt = super::paging::kernel_table();
        let mut pma = super::physical_memory_allocator();
        let num_pages = (layout.size() + ALLOC_HEADER_SIZE).div_ceil(PAGE_SIZE);
        let old_heap_size = self
            .heap_size
            .fetch_add(num_pages, core::sync::atomic::Ordering::AcqRel);
        let pages = pma
            .alloc_contig(num_pages)
            .expect("allocate pages for kernel heap");
        let old_heap_end = VirtualAddress(KERNEL_HEAP_START.0 + old_heap_size * PAGE_SIZE);
        pt.map_range(pages, old_heap_end, num_pages, true)
            .expect("map kernel heap pages");
        let block: *mut FreeBlock = old_heap_end.as_ptr();
        unsafe {
            let b = block.as_mut().unwrap();
            b.next = null_mut();
            b.prev = null_mut();
            b.size = num_pages;
        }
        *self.free_list_head.lock() = block;
    }

    fn log_heap_info(&self) {
        let heap_size = self.heap_size.load(core::sync::atomic::Ordering::Acquire);
        log::info!(
            "total heap size = {} pages ({} bytes)",
            heap_size,
            heap_size * PAGE_SIZE
        );
        let mut free_size = 0;
        unsafe {
            let mut cur = *self.free_list_head.lock();
            while let Some(cur_block) = cur.as_ref() {
                log::info!("0x{:x}: {cur_block:?}", cur as usize);
                free_size += cur_block.size;
                cur = cur_block.next;
            }
        }
        log::info!(
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
        self.add_free_block(
            ptr.offset(-(ALLOC_HEADER_SIZE as isize)),
            layout.size() + ALLOC_HEADER_SIZE,
        )
    }
}

pub fn log_heap_info() {
    GLOBAL_HEAP.log_heap_info();
}
