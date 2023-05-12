use core::{
    borrow::Borrow,
    ops::{Index, Range},
    ptr::NonNull,
};

use super::{
    paging, physical_memory_allocator, virtual_address_allocator, MemoryError, PhysicalAddress,
    VirtualAddress, PAGE_SIZE,
};

// TODO: could also have a PhysicalBox<T> type for typed physically accessible memory

/// PhysicalBuffer is an owned, contiguous region of memory with a known physical address that is
/// also mapped into the kernel's virtual address space
pub struct PhysicalBuffer {
    phy_addr: PhysicalAddress,
    vir_addr: VirtualAddress,
    page_count: usize,
}

impl PhysicalBuffer {
    pub fn alloc(
        page_count: usize,
        page_table_options: &paging::PageTableEntryOptions,
    ) -> Result<Self, MemoryError> {
        let base_address_phy = physical_memory_allocator().alloc_contig(page_count)?;
        let base_address_vir = virtual_address_allocator().alloc(page_count)?;
        paging::kernel_table()
            .map_range(
                base_address_phy,
                base_address_vir,
                page_count,
                true, // TODO: right now this crashes, probably because the overwrite check is broken
                page_table_options,
            )
            .expect("map physical buffer memory should succeed because VA came from VA allocator");
        Ok(Self {
            phy_addr: base_address_phy,
            vir_addr: base_address_vir,
            page_count,
        })
    }

    pub fn physical_address(&self) -> PhysicalAddress {
        self.phy_addr
    }

    pub fn virtual_address(&self) -> VirtualAddress {
        self.vir_addr
    }

    pub fn page_count(&self) -> usize {
        self.page_count
    }

    /// length in bytes
    pub fn len(&self) -> usize {
        self.page_count * PAGE_SIZE
    }

    fn unmap_virtual(&self) {
        paging::kernel_table().unmap_range(self.vir_addr, self.page_count);
        virtual_address_allocator().free(self.vir_addr, self.page_count);
    }

    /// Unmap this buffer from the kernel's virtual address space, returning the physical address and size
    /// The caller is responsible for returning the physical pages to the physical_memory_allocator
    pub fn unmap(self) -> (PhysicalAddress, usize) {
        self.unmap_virtual();
        (self.phy_addr, self.page_count)
    }

    pub fn as_bytes(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.vir_addr.as_ptr::<u8>(), self.len()) }
    }

    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.vir_addr.as_ptr::<u8>(), self.len()) }
    }
}

impl Drop for PhysicalBuffer {
    fn drop(&mut self) {
        self.unmap_virtual();
        physical_memory_allocator().free_pages(self.phy_addr, self.page_count);
    }
}
