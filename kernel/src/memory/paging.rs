#![allow(unused)]
use core::{alloc::Allocator, ops::Range};

use alloc::vec::Vec;

use super::{PhysicalAddress, VirtualAddress, PAGE_SIZE, PhysicalMemoryAllocator, MemoryError};

struct PageTableEntry {}

impl PageTableEntry {
    fn table_desc(table_ptr: PhysicalAddress) -> PageTableEntry {
        todo!()
    }

    fn block_entry(base_address: PhysicalAddress) -> PageTableEntry {
        todo!()
    }

    fn entry(base_address: PhysicalAddress) -> PageTableEntry {
        todo!()
    }

    fn invalid() -> PageTableEntry {
        todo!()
    }

    /// Return a reference to the next level table that this entry refers to, or None if the entry
    /// is un-allocated or otherwise does not refer to a level table
    fn table_ref(&self) -> Option<&mut LevelTable> {
        // TODO: this function is quite complex, because we need to figure out the address of the
        // table in the current VIRTUAL memory space but we only have the PHYSICAL base address.
        // One potential solution is to identity map (by necessity in the kernel's low address range)
        // all page tables, so that the physical and virtual addresses are always the same. Perhaps
        // that is jank, but it is *really* convenient. Potentially not so great for the TLB though?
        todo!()
    }

    fn base_address(&self) -> Option<usize> {
        todo!()
    }

    fn allocated(&self) -> bool {
        todo!()
    }

    pub fn table_phy_addr(&self) -> PhysicalAddress {
        todo!()
    }
}

struct LevelTable {
    entries: [PageTableEntry; 512],
}

impl core::ops::Index<usize> for LevelTable {
    type Output = PageTableEntry;

    fn index(&self, index: usize) -> &Self::Output {
        &self.entries[index]
    }
}

impl core::ops::IndexMut<usize> for LevelTable {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.entries[index]
    }
}

// WARN: Currently assumes that physical memory is identity mapped in the 0x0000 prefix!
pub struct PageTable<'a> {
    /// true => this page table is for virtual addresses of prefix 0xffff, false => prefix must be 0x0000
    high_addresses: bool,
    phys_page_alloc: &'a mut PhysicalMemoryAllocator,
    level0_table: &'a mut LevelTable,
    level0_phy_addr: PhysicalAddress
}

pub enum MapError {
    RangeAlreadyMapped {
        virt_start: VirtualAddress,
        page_count: usize,
        collision: VirtualAddress,
    },
    MapRangeBadVirtualBase {
        reason: &'static str,
        virt_start: VirtualAddress,
    },
    InsufficentMapSpace {
        page_count: usize,
        virt_start: VirtualAddress,
    },
    Memory(MemoryError)
}

impl<'a> PageTable<'a> {
    pub fn empty(phys_page_alloc: &'a mut PhysicalMemoryAllocator, high_addresses: bool) -> Result<PageTable<'a>, MemoryError> {
        let level0_phy_addr = phys_page_alloc.alloc()?;
        let l0_ref = unsafe {
            // WARN: Assume that memory is identity mapped!
            let table_ptr: *mut LevelTable = VirtualAddress(level0_phy_addr.0).as_ptr();
            core::ptr::write_bytes(table_ptr, 0, core::mem::size_of::<LevelTable>());
            &mut *table_ptr
        };
        Ok(PageTable {
            high_addresses,
            phys_page_alloc,
            level0_table: l0_ref,
            level0_phy_addr
        })
    }

    pub fn identity(phys_page_alloc: &'a mut PhysicalMemoryAllocator, high_addresses: bool, start_addr: PhysicalAddress, page_count: usize) -> Result<PageTable<'a>, MapError> {
        let mut p = PageTable::empty(phys_page_alloc, high_addresses).map_err(MapError::Memory)?;
        p.map_range(start_addr, VirtualAddress(start_addr.0), page_count, true)?;
        Ok(p)
    }

    pub unsafe fn activate(&self) {
        todo!()
    }

    fn allocate_table(&self) -> Result<PhysicalAddress, MemoryError> {
        let new_page_phy_addr = self.phys_page_alloc.alloc()?;
        unsafe {
            // WARN: Assume that memory is identity mapped!
            let table_ptr: *mut LevelTable = VirtualAddress(new_page_phy_addr.0).as_ptr();
            core::ptr::write_bytes(table_ptr, 0, core::mem::size_of::<LevelTable>());
        }
        Ok(new_page_phy_addr)
    }

    pub fn map_range(
        &mut self,
        phy_start: PhysicalAddress,
        virt_start: VirtualAddress,
        page_count: usize,
        overwrite: bool,
    ) -> Result<(), MapError> {
        // determine how big page_count is relative to the size of the higher level page table blocks and figure out how many new tables we'll need, if any
        let num_l3_tables = page_count.div_ceil(512);
        let num_l2_tables = num_l3_tables.div_ceil(512);
        let num_l1_tables = num_l2_tables.div_ceil(512);

        // find spot in page table where virt_start is
        let (tag, lv0_start, lv1_start, lv2_start, lv3_start, page_offset) = virt_start.to_parts();
        if page_offset != 0 {
            return Err(MapError::MapRangeBadVirtualBase {
                reason: "starting virtual address for mapping range must be at a page boundary",
                virt_start,
            });
        }
        match (self.high_addresses, tag) {
            (true, 0xffff) | (false, 0x0) => {}
            _ => {
                return Err(MapError::MapRangeBadVirtualBase {
                    reason:
                        "starting virtual address for mapping range must match tag for page table",
                    virt_start,
                })
            }
        }

        if !overwrite {
            // TODO: this always takes linear time in the number of pages although the actual allocation
            // doesn't. They could probably both have the same runtime though with some cleverness.
            let mut page_index = 0;
            'top: while page_index < page_count {
                let page_start = VirtualAddress(virt_start.0 + page_index * PAGE_SIZE);
                let (tag, i0, i1, i2, i3, po) = page_start.to_parts();
                let mut table = self.level0_table;
                for i in [i0, i1, i2, i3] {
                    if let Some(existing_table) = table[i].table_ref() {
                        table = existing_table;
                    } else if table[i].allocated() {
                        return Err(MapError::RangeAlreadyMapped {
                            virt_start,
                            page_count,
                            collision: page_start,
                        });
                    }
                }
                page_index += 1;
            }
        }

        // allocate new page tables and add required entries
        let mut page_index = 0;
        'top: while page_index < page_count {
            let page_start = VirtualAddress(virt_start.0 + page_index * PAGE_SIZE);
            let (tag, i0, i1, i2, i3, po) = page_start.to_parts();
            let mut table = self.level0_table;
            // TODO: surely there is a clean way to generate this slice with iterators?
            for (i, can_allocate_large_block, large_block_size) in [
                (i0, false, 0),
                (i1, i2 == 0 && i3 == 0, 512 * 512), // 1GiB in pages
                (i2, i3 == 0, 512),                  // 2MiB in pages
            ] {
                if let Some(existing_table) = table[i].table_ref() {
                    table = existing_table;
                } else {
                    assert!(!table[i].allocated());
                    if can_allocate_large_block && (page_count - page_index) >= large_block_size {
                        table[i] = PageTableEntry::block_entry(PhysicalAddress(
                            phy_start.0 + page_index * PAGE_SIZE,
                        ));
                        page_index += large_block_size;
                        continue 'top;
                    } else {
                        table[i] = PageTableEntry::table_desc(self.allocate_table().map_err(MapError::Memory)?);
                        table = table[i].table_ref().unwrap();
                    }
                }
            }

            table[i3] =
                PageTableEntry::entry(PhysicalAddress(phy_start.0 + page_index * PAGE_SIZE));
            page_index += 1;
        }

        Ok(())
    }

    pub fn unmap_range(&mut self, virt_start: VirtualAddress, page_count: usize) {
        let mut page_index = 0;
        'top: while page_index < page_count {
            let page_addr = VirtualAddress(virt_start.0 + page_index * PAGE_SIZE);
            let (tag, i0, i1, i2, i3, po) = page_addr.to_parts();
            let mut table = self.level0_table;
            // TODO: surely there is a clean way to generate this slice with iterators?
            for (i, block_size) in [
                (i0, 0),
                (i1, 512 * 512), // 1GiB in pages
                (i2, 512),                  // 2MiB in pages
                (i3, 1)
            ] {
                if let Some(existing_table) = table[i].table_ref() {
                    table = existing_table;
                } else {
                    assert_ne!(block_size, 0);
                    table[i] = PageTableEntry::invalid();
                    page_index += block_size;
                    break;
                }
            }
        }
        todo!()
    }

    /// Compute the physical address of a virtual address the same way the MMU would. If accessing
    /// the virtual address would result in a page fault, `None` is returned
    pub fn physical_address_of(&self, virt: VirtualAddress) -> Option<PhysicalAddress> {
        let (tag, lv0_index, lv1_index, lv2_index, lv3_index, page_offset) = virt.to_parts();

        match (self.high_addresses, tag) {
            (true, 0xffff) | (false, 0x0) => {}
            _ => return None,
        }

        self.level0_table[lv0_index]
            .table_ref()
            .and_then(|lv1_table| {
                lv1_table[lv1_index].table_ref().and_then(|lv2_table| {
                    lv2_table[lv2_index].table_ref().and_then(|lv3_table| {
                        lv3_table[lv3_index]
                            .base_address()
                            .map(|base_addr| PhysicalAddress(base_addr + page_offset))
                    })
                })
            })
    }

    pub fn virtual_address_of(&self, phy: PhysicalAddress) -> Option<VirtualAddress> {
        todo!()
    }
}

impl Drop for PageTable<'_> {
    fn drop(&mut self) {
        // Does not drop actual pages!
        fn drop_table(table: &mut LevelTable, table_phy_addr: PhysicalAddress, src_alloc: &mut PhysicalMemoryAllocator) {
            for entry in table.entries {
                if let Some(table) = entry.table_ref() {
                    drop_table(table, entry.table_phy_addr(), src_alloc);
                }
            }
            src_alloc.free(table_phy_addr)
        }
        drop_table(self.level0_table, self.level0_phy_addr, self.phys_page_alloc);
    }
}
