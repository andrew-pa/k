#![allow(unused)]
use core::{alloc::Allocator, ops::Range};

use alloc::vec::Vec;

use super::{PhysicalAddress, VirtualAddress};

struct PageTableEntry {}

impl PageTableEntry {
    fn table_ref(&self) -> Option<&LevelTable> {
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
        todo!()
    }
    pub fn unmap_range(&mut self, virt_start: VirtualAddress, page_count: usize) {
        todo!()
    }

    /// Compute the physical address of a virtual address the same way the MMU would. If accessing
    /// the virtual address would result in a page fault, `None` is returned
    pub fn physical_address_of(&self, virt: VirtualAddress) -> Option<PhysicalAddress> {
        let tag = 0xffff_0000_0000_0000_0000 & virt.0;
        match (self.high_addresses, tag) {
            (true, 0xffff_0000_0000_0000_0000) | (false, 0x0) => {}
            _ => return None,
        }

        let lv0_index = ((0x1ff << 39) & virt.0) >> 39;
        let lv1_index = ((0x1ff << 30) & virt.0) >> 30;
        let lv2_index = ((0x1ff << 21) & virt.0) >> 21;
        let lv3_index = ((0x1ff << 12) & virt.0) >> 12;
        let page_offset = virt.0 & 0x3ff;

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
