#![allow(unused)]
use core::{alloc::Allocator, ops::Range};

use alloc::vec::Vec;

use super::{PhysicalAddress, VirtualAddress};

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
    RangeAlreadyMapped,
    MapRangeBadVirtualBase {
        reason: &'static str,
        virt_start: VirtualAddress,
    }
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
                virt_start
            });
        }
        match (self.high_addresses, tag) {
            (true, 0xffff_0000_0000_0000_0000) | (false, 0x0) => {}
            _ => return Err(MapError::MapRangeBadVirtualBase {
                reason: "starting virtual address for mapping range must match tag for page table",
                virt_start
            })
        }

        // check to make sure there is enough space
        for lv0_index in lv0_start..(lv0_start + num_l1_tables) {
            match self.level_tables[0][lv0_index].table_ref() {
                Some(lv1_table) => for lv1_index in lv1_start..(lv1_start + num_l2_tables) {
                    match lv1_table[lv1_index].table_ref() {
                        Some(lv2_table) => for lv2_index in lv2_start..(lv2_start + num_l3_tables) {
                            match lv2_table[lv2_index].table_ref() {
                                Some(lv3_table) => {
                                    // if there is a table, then does it have enough space?
                                },
                                None => {
                                    // if there's no table there, that's fine, we'll allocate one
                                },
                            }
                        },
                        None => {
                            // there's not even a L2 table allocated for this address range, so we're good
                        }
                    }
                },
                None => {
                    // there's not even a L1 table allocated for this address range, so we're good
                }
            }
        }

        // allocate new page tables and add required entries
        todo!()
    }

    pub fn unmap_range(&mut self, virt_start: VirtualAddress, page_count: usize) {
        todo!()
    }

    /// Compute the physical address of a virtual address the same way the MMU would. If accessing
    /// the virtual address would result in a page fault, `None` is returned
    pub fn physical_address_of(&self, virt: VirtualAddress) -> Option<PhysicalAddress> {
        let (tag, lv0_index, lv1_index, lv2_index, lv3_index, page_offset) =
            virt.to_parts();

        match (self.high_addresses, tag) {
            (true, 0xffff_0000_0000_0000_0000) | (false, 0x0) => {}
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
}
