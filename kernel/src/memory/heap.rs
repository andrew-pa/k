//! The kernel Rust heap allocator, implements [GlobalAlloc].
//!
//! The allocator is a simple free list style allocator. Free blocks are stored in the unused
//! memory of the heap.
use core::{
    alloc::{GlobalAlloc, Layout},
    mem::size_of,
    ptr::null_mut,
    sync::atomic::AtomicBool,
};

use bitfield::bitfield;
use spin::Mutex;

use crate::memory::paging::PageTableEntryOptions;

use super::{VirtualAddress, PAGE_SIZE};

const KERNEL_HEAP_START: VirtualAddress = VirtualAddress(0xffff_ff00_0000_0000);
const INIT_SIZE: usize = 8; // pages

/*struct AllocatedBlockHeader {
    size: usize,
}*/
bitfield! {
    /// An allocated block header stores the size of the allocated block, and the amount of padding that preceeds this header.
    struct AllocatedBlockHeader(u64);
    // TODO: we could probably use less bits here, if needed
    u8, padding, set_padding: 63,56;
    size, set_size: 55, 0;
}

impl AllocatedBlockHeader {
    fn new(size: usize, padding: usize) -> Self {
        let mut s = AllocatedBlockHeader(0);
        s.set_size(size as u64);
        s.set_padding(padding as u8);
        s
    }

    fn padding_size(&self) -> isize {
        self.padding() as isize
    }
}

#[derive(Debug)]
struct FreeBlockHeader {
    size: usize,
    next: *mut FreeBlockHeader,
    prev: *mut FreeBlockHeader,
}

#[derive(Debug)]
struct FreeBlock {
    address: VirtualAddress,
    size: usize,
}

enum BlockAdjacency {
    /// end of self is beginning of other
    Before,
    /// beginning of self is end of other
    After,
    NotAdjacent,
}

impl FreeBlock {
    fn check_adjacency(&self, other: &FreeBlock) -> BlockAdjacency {
        if self.address.add(self.size) == other.address {
            BlockAdjacency::Before
        } else if self.address == other.address.add(other.size) {
            BlockAdjacency::After
        } else {
            BlockAdjacency::NotAdjacent
        }
    }
}

// SAFETY: although we can try to make this as safe as possible, because we give users raw
// pointers into the heap they can always accidently overwrite a header and cause unsafe behavior
struct FreeList {
    head: *mut FreeBlockHeader,
    tail: *mut FreeBlockHeader,
    heap_size_in_bytes: usize,
}

struct FreeListCursor<'a> {
    current: *mut FreeBlockHeader,
    parent: &'a mut FreeList,
}

impl FreeList {
    const fn new_uninit() -> Self {
        FreeList {
            head: null_mut(),
            tail: null_mut(),
            heap_size_in_bytes: 0,
        }
    }

    // SAFETY: assumes that at least the page at KERNEL_HEAP_START has already been allocated and mapped
    unsafe fn init(&mut self, initial_size_in_bytes: usize) {
        assert!(self.head.is_null());
        self.head = KERNEL_HEAP_START.as_ptr();
        *self.head = FreeBlockHeader {
            size: initial_size_in_bytes,
            next: null_mut(),
            prev: null_mut(),
        };
        self.tail = self.head;
        self.heap_size_in_bytes = initial_size_in_bytes;
    }

    fn cursor(&mut self) -> FreeListCursor<'_> {
        let c = FreeListCursor {
            current: self.head,
            parent: self,
        };
        // TODO: inelegant
        c.check_ptr(c.current);
        c
    }

    fn increase_size(&mut self, new_size: usize) {
        self.heap_size_in_bytes += new_size;
    }
}

impl<'a> FreeListCursor<'a> {
    fn current(&self) -> Option<FreeBlock> {
        unsafe {
            self.current.as_ref().map(|pb| FreeBlock {
                address: VirtualAddress::from(self.current),
                size: pb.size,
            })
        }
    }

    /// Remove the current block, moving the cursor forward, or remaining at the head
    fn remove_current(&mut self) -> Option<FreeBlock> {
        log::trace!("removing {:?}", self.current());
        // SAFETY: current should be checked, and we check all other accessed pointers
        unsafe {
            let removed_block_p = self.current;
            removed_block_p.as_ref().map(|removed_block| {
                self.check_ptr(removed_block.prev);
                match removed_block.prev.as_mut() {
                    // removed block is in the middle of the list
                    Some(before_removed_block) => {
                        self.check_ptr(removed_block.next);
                        if let Some(after_removed_block) = removed_block.next.as_mut() {
                            assert_eq!(after_removed_block.prev, removed_block_p);
                            after_removed_block.prev = removed_block.prev;
                        }
                        before_removed_block.next = removed_block.next;
                        self.current = before_removed_block.next;
                        if self.parent.tail == removed_block_p {
                            self.parent.tail = before_removed_block;
                        }
                    }
                    // removed block is the head
                    None => {
                        assert_eq!(removed_block_p, self.parent.head);
                        self.parent.head = removed_block.next;
                        self.check_ptr(self.parent.head);
                        if let Some(new_head_block) = self.parent.head.as_mut() {
                            assert_eq!(new_head_block.prev, removed_block_p);
                            new_head_block.prev = null_mut()
                        }
                        self.current = self.parent.head;
                        if self.parent.tail == removed_block_p {
                            self.parent.tail = self.parent.head;
                        }
                    }
                }
                FreeBlock {
                    address: VirtualAddress::from(removed_block_p),
                    size: removed_block.size,
                }
            })
        }
    }

    /// best effort check to make sure a pointer is valid
    fn check_ptr(&self, ptr: *mut FreeBlockHeader) {
        if !ptr.is_null() {
            let addr = ptr as usize;
            // addr must be somewhere in the heap
            // assume that any address in this range is valid, which it should be given that the heap physical page allocation and mapping code is valid
            assert!(
                addr >= KERNEL_HEAP_START.0
                    && addr <= KERNEL_HEAP_START.0 + self.parent.heap_size_in_bytes,
                "address to free block out of range: 0x{:x} <= 0x{:x} <= 0x{:x}",
                KERNEL_HEAP_START.0,
                addr,
                KERNEL_HEAP_START.0 + self.parent.heap_size_in_bytes
            );
        }
    }

    fn move_next(&mut self) {
        // SAFETY: cursor current pointer should always be null or valid
        unsafe {
            if let Some(b) = self.current.as_ref() {
                self.current = b.next
            }
        }
        self.check_ptr(self.current);
    }

    /// insert a new block before the cursor's current block, leaving the cursor unmoved
    fn insert_before_current(&mut self, new_block: FreeBlock) {
        log::trace!("insert {new_block:?} before {:?}", self.current());
        assert_ne!(new_block.size, 0);
        // (current.prev) <-> new_block <-> current
        // SAFETY: list ptrs should be good already and we check the new_block
        unsafe {
            // setup block header in memory
            let new_block_ptr: *mut FreeBlockHeader = new_block.address.as_ptr();
            self.check_ptr(new_block_ptr);
            let header = new_block_ptr.as_mut().expect("block address non-null");
            header.size = new_block.size;

            if self.current == self.parent.head {
                header.next = self.parent.head;
                header.prev = null_mut();
                if let Some(head) = self.parent.head.as_mut() {
                    head.prev = new_block_ptr;
                }
                // if the list was previously empty, make sure the tail pointer is correctly updated
                if self.parent.tail.is_null() {
                    self.parent.tail = new_block_ptr;
                }
                self.parent.head = new_block_ptr;
            } else {
                match self.current.as_mut() {
                    Some(current_node) => {
                        header.next = self.current;
                        header.prev = current_node.prev;
                        if let Some(prev) = current_node.prev.as_mut() {
                            prev.next = new_block_ptr;
                        }
                        current_node.prev = new_block_ptr;
                    }
                    // we're at the end of the list
                    None => {
                        self.check_ptr(self.parent.tail);
                        match self.parent.tail.as_mut() {
                            Some(old_tail) => {
                                assert!(old_tail.next.is_null());
                                old_tail.next = new_block_ptr;
                                header.next = null_mut();
                                header.prev = self.parent.tail;
                                self.parent.tail = new_block_ptr;
                            }
                            // the list is empty
                            None => {
                                assert!(
                                    self.parent.head.is_null(),
                                    "self.parent.head = 0x{:x} != null {new_block:?}",
                                    self.parent.head as usize
                                );
                                self.parent.head = new_block_ptr;
                                self.parent.tail = new_block_ptr;
                                header.next = null_mut();
                                header.prev = null_mut();
                            }
                        }
                    }
                }
            }
        }
    }

    fn extend_current(&self, change_in_size: usize) {
        // SAFETY: cursor current pointer should always be null or valid
        unsafe {
            if let Some(b) = self.current.as_mut() {
                b.size += change_in_size
            }
        }
    }
}

struct KernelGlobalAlloc {
    free_list: Mutex<FreeList>,
    initialized: AtomicBool, // TODO: original heap_size was in units of pages
}

unsafe impl Sync for KernelGlobalAlloc {}

#[global_allocator]
static GLOBAL_HEAP: KernelGlobalAlloc = KernelGlobalAlloc {
    free_list: Mutex::new(FreeList::new_uninit()),
    initialized: AtomicBool::new(false),
};

impl KernelGlobalAlloc {
    fn init(&self) {
        let was_init = self
            .initialized
            .swap(true, core::sync::atomic::Ordering::SeqCst);
        assert!(!was_init, "heap initialization should only occur once");
        log::info!("initializing kernel heap");
        let mut pt = super::paging::kernel_table();
        let pages = {
            // drop this early to make sure that the page table can also allocate physical memory
            let mut pma = super::physical_memory_allocator();
            pma.alloc_contig(INIT_SIZE)
                .expect("allocate pages for kernel heap")
        };
        log::trace!("initial physical pages at {pages}");
        pt.map_range(
            pages,
            KERNEL_HEAP_START,
            INIT_SIZE,
            true,
            &PageTableEntryOptions::default(),
        )
        .expect("map kernel heap pages");
        // SAFETY: the previous code should have set up memory correctly to call init()
        unsafe {
            self.free_list.lock().init(INIT_SIZE * PAGE_SIZE);
        }
    }

    fn ensure_init(&self) {
        if !self.initialized.load(core::sync::atomic::Ordering::SeqCst) {
            self.init();
        }
    }

    fn find_suitable_free_block(&self, layout: &Layout) -> Option<FreeBlock> {
        // this code is only in this function because we assume that we always allocate
        // first before free() is ever called
        self.ensure_init();

        let mut free_list = self.free_list.lock();

        let mut cursor = free_list.cursor();
        while let Some(block) = cursor.current() {
            // compute total size of the block including header and padding
            let required_size = layout.size()
                + size_of::<AllocatedBlockHeader>()
                + block
                    .address
                    .add(size_of::<AllocatedBlockHeader>())
                    .align_offset(layout.align());

            if block.size >= required_size {
                // we can use this block!
                return cursor.remove_current();
            }

            cursor.move_next();
        }
        // we got to the end of the list but never found a good block
        None
    }

    fn add_free_block(&self, mut new_block: FreeBlock) {
        // TODO: return pages back to the PMA?
        let mut free_list = self.free_list.lock();
        let mut cursor = free_list.cursor();
        while let Some(block) = cursor.current() {
            match new_block.check_adjacency(&block) {
                // merge block if it is adjacent
                // TODO: it's possible that the resulting block is now adjacent
                // with its neighbors
                BlockAdjacency::Before => {
                    cursor.remove_current();
                    new_block.size += block.size;
                    cursor.insert_before_current(new_block);
                    return;
                }
                BlockAdjacency::After => {
                    cursor.extend_current(new_block.size);
                    return;
                }
                // insert the block here to maintain sorted order
                // we know that no merges will be possible past this point
                BlockAdjacency::NotAdjacent => {
                    if new_block.address < block.address {
                        break;
                    }
                }
            }
            cursor.move_next();
        }
        // insert the new block as it was unmergable (possibly at the end)
        cursor.insert_before_current(new_block);
    }

    fn increase_heap_size(&self, layout: &Layout) {
        let mut free_list = self.free_list.lock();
        let num_new_pages = (free_list.heap_size_in_bytes + free_list.heap_size_in_bytes / 2)
            .max(layout.size())
            .div_ceil(PAGE_SIZE);
        let pages = {
            let mut pma = super::physical_memory_allocator();
            pma.alloc_contig(num_new_pages)
                .expect("allocate pages for kernel heap")
        };
        let old_heap_end = VirtualAddress(KERNEL_HEAP_START.0 + free_list.heap_size_in_bytes);
        {
            let mut pt = super::paging::kernel_table();
            pt.map_range(
                pages,
                old_heap_end,
                num_new_pages,
                true,
                &PageTableEntryOptions::default(),
            )
            .expect("map kernel heap pages");
        }
        log::trace!("increasing heap size by {}b", num_new_pages * PAGE_SIZE);
        free_list.increase_size(num_new_pages * PAGE_SIZE);
        drop(free_list);
        self.add_free_block(FreeBlock {
            address: old_heap_end,
            size: num_new_pages * PAGE_SIZE,
        });
    }

    fn log_heap_info(&self, level: log::Level) {
        if log::log_enabled!(level) {
            let mut free_list = self.free_list.lock();
            log::log!(
                level,
                "total heap size = {} pages ({} bytes)",
                free_list.heap_size_in_bytes.div_ceil(PAGE_SIZE),
                free_list.heap_size_in_bytes
            );
            let mut free_size = 0;
            let mut cur = free_list.cursor();
            while let Some(cur_block) = cur.current() {
                log::log!(level, "{:?}", cur_block);
                free_size += cur_block.size;
                cur.move_next();
            }
            log::log!(
                level,
                "total free = {free_size}b, total allocated = {}b",
                free_list.heap_size_in_bytes - free_size
            );
        }
    }
}

unsafe impl GlobalAlloc for KernelGlobalAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        log::trace!("alloc {layout:?}");
        self.log_heap_info(log::Level::Trace);
        // find and remove a free block that is >= size
        if let Some(block) = self.find_suitable_free_block(&layout) {
            // make it allocated, returning any extra back to the free list
            let padding = block
                .address
                .add(size_of::<AllocatedBlockHeader>())
                .align_offset(layout.align());
            log::trace!("found block {block:?}, required padding = {padding}",);
            let req_block_size = (layout.size() + size_of::<AllocatedBlockHeader>() + padding)
                // we can't make a block any smaller than this or freeing the block
                // will overwrite the next block
                .max(size_of::<FreeBlockHeader>());
            let actual_block_size = if block.size - req_block_size > size_of::<FreeBlockHeader>() {
                // put back the unused part of the block at the end
                self.add_free_block(FreeBlock {
                    address: block.address.add(req_block_size),
                    size: block.size - req_block_size,
                });
                req_block_size
            } else {
                // use the whole block
                block.size
            };
            assert!(actual_block_size >= size_of::<FreeBlockHeader>());
            let padded_address = block.address.add(padding);
            // write the allocated block header
            let header: *mut AllocatedBlockHeader = padded_address.as_ptr();
            unsafe {
                *header.as_mut().expect("p not null") =
                    AllocatedBlockHeader::new(actual_block_size, padding);
            }
            // return ptr to new allocated block
            let data = padded_address
                .as_ptr::<u8>()
                .add(size_of::<AllocatedBlockHeader>());
            log::trace!(
                "new allocated block @ {} (data @ {}), size = {}",
                block.address,
                VirtualAddress::from(data),
                actual_block_size
            );
            data
        } else {
            self.increase_heap_size(&layout);
            // try to allocate again
            self.alloc(layout)
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        log::trace!("dealloc 0x{:x} {layout:?}", ptr as usize);
        self.log_heap_info(log::Level::Trace);
        if ptr.is_null() {
            log::warn!("attempted to free nullptr with layout {layout:?}");
            return;
        }
        // TODO: we never return physical memory back to the system once it has been allocated, it just goes back in the heap free pool
        let header = ptr.offset(-((size_of::<AllocatedBlockHeader>()) as isize))
            as *mut AllocatedBlockHeader;
        let header_ref = header.as_ref().unwrap();
        let block_size = header_ref.size() as usize;
        let block_address = VirtualAddress::from(header).offset(-header_ref.padding_size());
        log::trace!(
            "block header for {:x} at {:x} (block address {block_address})",
            ptr as usize,
            header as usize
        );
        if layout.size() > block_size {
            self.log_heap_info(log::Level::Error);
            assert!(
                layout.size() <= block_size,
                "{layout:?}.size <= block_size@{block_size}"
            );
        }
        self.add_free_block(FreeBlock {
            address: block_address,
            size: block_size,
        });
        self.log_heap_info(log::Level::Trace);
    }
}

/// Write information about the global kernel heap to the log.
pub fn log_heap_info(level: log::Level) {
    GLOBAL_HEAP.log_heap_info(level);
}
