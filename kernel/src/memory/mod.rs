use derive_more::Display;

// these are defined by the linker script so we know where the BSS section is
// notably not the value but the *address* of these symbols is what is relevant
extern "C" {
    static mut __bss_start: u8;
    static mut __bss_end: u8;
    static mut __kernel_start: u8;
    static mut __kernel_end: u8;
}

/// Zero the BSS section of the kernel as is expected by the ELF
pub unsafe fn zero_bss_section() {
    let bss_start = &mut __bss_start as *mut u8;
    let bss_end = &mut __bss_end as *mut u8;
    let bss_size = bss_end.offset_from(bss_start) as usize;
    core::ptr::write_bytes(bss_start, 0, bss_size);
}

// TODO: large pages are most likely better
pub const PAGE_SIZE: usize = 4 * 1024;

#[derive(Copy, Clone, Debug, Display)]
#[display(fmt = "p:0x{:x}", _0)]
pub struct PhysicalAddress(pub usize);

#[derive(Copy, Clone, Debug, Display)]
#[display(fmt = "v:0x{:x}", _0)]
pub struct VirtualAddress(pub usize);

#[derive(Debug, Display)]
pub enum MemoryError {
    #[display(fmt = "out of memory")]
    OutOfMemory,
    #[display(fmt = "insufficent memory for allocation of {} bytes", size)]
    InsufficentForAllocation { size: usize },
}

mod physical_memory_allocator;
pub use physical_memory_allocator::PhysicalMemoryAllocator;

mod paging;
