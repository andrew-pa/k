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
// however, for now we are assuming 4kB pages throughout the kernel
// Linus apparently prefers 4kB, although that may be because it makes his life easier
// M1 Macs use 16kB pages though I think?? Switching is probably not that hard for a from-scratch project
pub const PAGE_SIZE: usize = 4 * 1024;

#[derive(Copy, Clone, Display, PartialEq, Eq)]
#[display(fmt = "p:0x{:x}", _0)]
pub struct PhysicalAddress(pub usize);

#[derive(Copy, Clone, Display, PartialEq, Eq)]
#[display(fmt = "v:0x{:x}", _0)]
pub struct VirtualAddress(pub usize);

impl PhysicalAddress {
    //TODO: WARN: assumes that we have identity mapped memory starting at 0xffff_0000_0000_0000 and that this physical address is part of the range of memory that has been identity mapped
    pub unsafe fn to_virtual_canonical(self) -> VirtualAddress {
        VirtualAddress(self.0 + 0xffff_0000_0000_0000)
    }
}

impl VirtualAddress {
    #[inline]
    pub fn to_parts(&self) -> (usize, usize, usize, usize, usize, usize) {
        let tag = (0xffff_0000_0000_0000 & self.0) >> 48;
        let lv0_index = ((0x1ff << 39) & self.0) >> 39;
        let lv1_index = ((0x1ff << 30) & self.0) >> 30;
        let lv2_index = ((0x1ff << 21) & self.0) >> 21;
        let lv3_index = ((0x1ff << 12) & self.0) >> 12;
        let page_offset = self.0 & 0x3ff;

        (tag, lv0_index, lv1_index, lv2_index, lv3_index, page_offset)
    }

    pub fn from_parts(
        tag: usize,
        l0: usize,
        l1: usize,
        l2: usize,
        l3: usize,
        offset: usize,
    ) -> VirtualAddress {
        assert!(tag <= 0xffff);
        assert!(l0 <= 0x1ff);
        assert!(l1 <= 0x1ff);
        assert!(l2 <= 0x1ff);
        assert!(l3 <= 0x1ff);
        assert!(offset <= 0x3ff);
        VirtualAddress((tag << 48) | (l0 << 39) | (l1 << 30) | (l2 << 21) | (l3 << 12) | offset)
    }

    pub fn as_ptr<T>(&self) -> *mut T {
        self.0 as *mut T
    }

    //TODO: WARN: assumes that we have identity mapped memory starting at 0xffff_0000_0000_0000, this will only work if the address is in the identity mapped region
    pub unsafe fn to_physical_canonical(self) -> PhysicalAddress {
        PhysicalAddress(self.0 - 0xffff_0000_0000_0000)
    }
}

impl<T> From<*mut T> for VirtualAddress {
    fn from(value: *mut T) -> Self {
        VirtualAddress(value as usize)
    }
}

impl core::fmt::Debug for PhysicalAddress {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "p:0x{:x}", self.0)
    }
}

impl core::fmt::Debug for VirtualAddress {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "v:0x{:x}", self.0)
    }
}

#[derive(Debug, Display)]
pub enum MemoryError {
    #[display(fmt = "out of memory")]
    OutOfMemory,
    #[display(fmt = "insufficent memory for allocation of {} bytes", size)]
    InsufficentForAllocation { size: usize },
}

mod physical_memory_allocator;
pub use physical_memory_allocator::{
    init_physical_memory_allocator, physical_memory_allocator, PhysicalMemoryAllocator,
};

pub mod paging;
