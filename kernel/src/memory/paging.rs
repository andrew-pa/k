#![allow(unused)]
use core::{alloc::Allocator, ops::Range};

use alloc::vec::Vec;

use super::{PhysicalAddress, VirtualAddress, PAGE_SIZE};

struct PageTableEntry {}

impl PageTableEntry {
    /// Return a reference to the next level table that this entry refers to, or None if the entry
    /// is un-allocated or otherwise does not refer to a level table
    fn table_ref(&self) -> Option<&LevelTable> {
        // TODO: this function is quite complex, because we need to figure out the address of the
        // table in the current VIRTUAL memory space but we only have the PHYSICAL base address.
        // One potential solution is to identity map (ostensibly in the kernel's low address range)
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

    fn table_desc(table_ptr: PhysicalAddress) -> PageTableEntry {
        todo!()
    }

    fn block_desc(page_size: PhysicalAddress) -> PageTableEntry {
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

pub struct PageTable<A: Allocator> {
    /// true => this page table is for virtual addresses of prefix 0xffff, false => prefix must be 0x0000
    high_addresses: bool,
    level_tables: Vec<LevelTable, A>,
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
}

impl<A: Allocator> PageTable<A> {
    pub fn empty(alloc: A) -> PageTable<A> {
        todo!()
    }
    pub fn identity(alloc: A) -> PageTable<A> {
        todo!()
    }

    pub unsafe fn activate(&self) {
        todo!()
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
                let mut table = &self.level_tables[0];
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
            let mut table = &self.level_tables[0];
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
                        table[i] = PageTableEntry::block_desc(PhysicalAddress(
                            phy_start.0 + page_index * PAGE_SIZE,
                        ));
                        page_index += large_block_size;
                        continue 'top;
                    } else {
                        table[i] = PageTableEntry::table_desc(self.allocate_table());
                    }
                }
            }
            table[i2][i3] =
                PageTableEntry::entry(PhysicalAddress(phy_start.0 + page_index * PAGE_SIZE));
            page_index += 1;
        }

        Ok(())
    }

    pub fn unmap_range(&mut self, virt_start: VirtualAddress, page_count: usize) {
        let mut page_index = 0;
        'top: while page_index < page_count {
            let page_start = VirtualAddress(virt_start.0 + page_index * PAGE_SIZE);
            let (tag, i0, i1, i2, i3, po) = page_start.to_parts();
            let mut table = &self.level_tables[0];
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
                    assert_ne!(block_size, 0); // no blocks in Level 0 table
                    //?
                    page_index += block_size;
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

        self.level_tables[0][lv0_index]
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

    fn allocate_table(&self) -> PhysicalAddress {
        todo!()
    }
}
