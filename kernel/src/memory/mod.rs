
// these are defined by the linker script so we know where the BSS section is
extern "C" {
    static mut __bss_start: u8;
    static mut __bss_end: u8;
    static mut __kernel_end: u64;
}

/// Zero the BSS section of the kernel as is expected by the ELF
pub unsafe fn zero_bss_section() {
    let bss_start = &mut __bss_start as *mut u8;
    let bss_end   = &mut __bss_end as *mut u8;
    let bss_size = bss_end.offset_from(bss_start) as usize;
    core::ptr::write_bytes(bss_start, 0, bss_size);
}

pub const PAGE_SIZE: usize = 4 * 1024;

pub struct PhysicalAddress(pub usize);
pub struct VirtualAddress(pub usize);

mod physical_memory_allocator;
pub use physical_memory_allocator::PhysicalMemoryAllocator;
