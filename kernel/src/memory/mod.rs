//! Memory managment, paging, and allocation.
//!
//! There are three allocators:
//! - Physical memory allocator (for physical pages of RAM)
//! - Virtual memory allocator (for virtual pages of addresses that are unmapped, in the kernel
//! address space)
//! - The kernel heap, for Rust heap allocations in the kernel
//!
//! Most things that need virtual addresses assigned (like device drivers) should use the global
//! virtual address allocator to dynamically be assigned a range of addresses.
//!
//! # Kernel Virtual Memory Map
//! The kernel's virtual address space has the following structure:
//!
//! | Virtual Address Range         | Size | What's there               |
//! |-------------------------------|------|----------------------------|
//! | `0xffff_0000_0000_0000..0xffff_0000_ffff_ffff` | 4GiB | Identity mapped to first 4GiB of physical memory including the kernel code/data |
//! | `0xffff_0001_0000_0000..0xffff_0101_0000_0000` | 1TiB | Reserved for the global virtual address allocator ([virtual_address_allocator()]). Initially unmapped. |
//! | `0xffff_ff00_0000_0000..0xffff_ffff_ffff_ffff` | Dynamic, 256GiB maximum | Kernel Rust heap, allocated and mapped on demand |
//!

use bytemuck::{Pod, Zeroable};
use derive_more::Display;
use snafu::Snafu;

// these are defined by the linker script so we know where the BSS section is
// notably not the value but the *address* of these symbols is what is relevant
extern "C" {
    static mut __bss_start: u8;
    static mut __bss_end: u8;
    static mut __kernel_start: u8;
    static mut __kernel_end: u8;
}

/// Zero the BSS section of the kernel as is expected by the ELF
///
/// # Safety
/// This function should only be called *once* at the beginning of boot.
pub unsafe fn zero_bss_section() {
    let bss_start = core::ptr::addr_of_mut!(__bss_start);
    let bss_end = core::ptr::addr_of_mut!(__bss_end);
    let bss_size = bss_end.offset_from(bss_start) as usize;
    core::ptr::write_bytes(bss_start, 0, bss_size);
}

// TODO: large pages are most likely better
// however, for now we are assuming 4kB pages throughout the kernel
// Linus apparently prefers 4kB, although that may be because it makes his life easier
// M1 Macs use 16kB pages though I think?? Switching is probably not that hard for a from-scratch project
/// The size in bytes of a single page.
pub const PAGE_SIZE: usize = 4 * 1024;

/// A physical memory address.
///
/// These addresses are not directly dereferencable. The MMU outputs these addresses.
#[derive(Copy, Clone, Display, PartialEq, Eq, PartialOrd, Ord, Default, Pod, Zeroable, Hash)]
#[display(fmt = "p:0x{:x}", _0)]
#[repr(transparent)]
pub struct PhysicalAddress(pub usize);

/// A virtual memory address.
///
/// These addresses may be directly dereferencable, depending on the state of the page tables.
/// The MMU takes these as input.
#[derive(Copy, Clone, Display, PartialEq, Eq, PartialOrd, Ord, Default, Pod, Zeroable, Hash)]
#[display(fmt = "v:0x{:x}", _0)]
#[repr(transparent)]
pub struct VirtualAddress(pub usize);

impl PhysicalAddress {
    /// Convert a physical address to a virtual address that is "canonically" mapped into the
    /// kernel's address space.
    ///
    /// # Safety
    /// This function *assumes* that we have identity mapped memory starting at 0xffff_0000_0000_0000 and that this physical address is part of the range of memory that has been identity mapped at boot.
    /// This is currently a 4GiB region starting at p:0x4000_0000 (see `start.S`).
    pub unsafe fn to_virtual_canonical(self) -> VirtualAddress {
        VirtualAddress(self.0 + 0xffff_0000_0000_0000)
    }

    /// Returns true if this address is aligned on a page boundary.
    pub fn is_page_aligned(&self) -> bool {
        // self.0.trailing_zeros() == PAGE_SIZE.ilog2()
        self.0 & (PAGE_SIZE - 1) == 0
    }

    /// Compute the address starting at self and adding `byte_offset`, saturating at the maximum.
    #[allow(clippy::should_implement_trait)]
    pub fn add(self, byte_offset: usize) -> PhysicalAddress {
        PhysicalAddress(self.0.saturating_add(byte_offset))
    }

    /// Compute the address starting at self and adding `byte_offset`, saturating at the maximum.
    pub fn offset(self, byte_offset: isize) -> PhysicalAddress {
        PhysicalAddress(self.0.saturating_add_signed(byte_offset))
    }
}

impl VirtualAddress {
    /// Break a virtual address into the page table indices and a page offset.
    #[inline]
    pub fn to_parts(&self) -> (usize, usize, usize, usize, usize, usize) {
        let tag = (0xffff_0000_0000_0000 & self.0) >> 48;
        let lv0_index = ((0x1ff << 39) & self.0) >> 39;
        let lv1_index = ((0x1ff << 30) & self.0) >> 30;
        let lv2_index = ((0x1ff << 21) & self.0) >> 21;
        let lv3_index = ((0x1ff << 12) & self.0) >> 12;
        let page_offset = self.0 & 0xfff;

        (tag, lv0_index, lv1_index, lv2_index, lv3_index, page_offset)
    }

    /// Reconstitute a virtual address from the page table indices and page offset.
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

    /// Convert this virtual address into a pointer.
    pub fn as_ptr<T>(&self) -> *mut T {
        self.0 as *mut T
    }

    /// Convert this virtual address into a physical address, assuming the "canonical" mapping (ie the mapping the kernel `text` section is mapped under).
    ///
    /// This function *assumes* that we have identity mapped memory starting at 0xffff_0000_0000_0000 and that this physical address is part of the range of memory that has been identity mapped at boot.
    /// This is currently a 4GiB region starting at p:0x4000_0000 (see `start.S`).
    pub fn to_physical_canonical(self) -> PhysicalAddress {
        PhysicalAddress(self.0 - 0xffff_0000_0000_0000)
    }

    /// Compute the address starting at self and adding `byte_offset`, wrapping at the maximum.
    // TODO: this one should be unsigned/forward only and we should have a different name for the
    // bidirectional offset, since we do forward offsets much much more often
    pub fn offset(self, byte_offset: isize) -> VirtualAddress {
        // TODO: is wrapping right or should we panic on overflow?
        VirtualAddress(self.0.wrapping_add_signed(byte_offset))
    }

    /// Compute the address starting at self and adding `byte_offset`, wrapping at the maximum.
    #[allow(clippy::should_implement_trait)]
    pub fn add(self, byte_offset: usize) -> VirtualAddress {
        // TODO: is wrapping right or should we panic on overflow?
        VirtualAddress(self.0.wrapping_add(byte_offset))
    }

    /// Compute the alignment offset for `alignment` to make this address aligned at that alignment, in bytes.
    pub fn align_offset(&self, alignment: usize) -> usize {
        self.as_ptr::<u8>().align_offset(alignment)
    }
}

impl<T> From<*const T> for VirtualAddress {
    fn from(value: *const T) -> Self {
        VirtualAddress(value as usize)
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

/// An error resulting from a memory operation.
#[derive(Debug, Snafu)]
pub enum MemoryError {
    /// Out of memory.
    OutOfMemory,
    #[snafu(display("insufficent memory for allocation of {size} bytes"))]
    InsufficentForAllocation { size: usize },
    /// Error from manipulating page tables.
    Map { source: paging::MapError },
}

mod physical_memory_allocator;
pub use physical_memory_allocator::{
    init_physical_memory_allocator, physical_memory_allocator, PhysicalMemoryAllocator,
};

pub mod heap;
pub mod paging;

mod virtual_address_allocator;
pub use virtual_address_allocator::{
    init_virtual_address_allocator, virtual_address_allocator, VirtualAddressAllocator,
};

mod physical_buffer;
pub use physical_buffer::PhysicalBuffer;
