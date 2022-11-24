#![allow(unused)]
use core::ops::Range;

use bitfield::bitfield;

use super::{
    physical_memory_allocator, MemoryError, PhysicalAddress, PhysicalMemoryAllocator,
    VirtualAddress, PAGE_SIZE,
};

bitfield! {
    pub struct PageTableEntry(u64);
    u8;
    valid, set_valid: 0;
    get_type, set_type: 1;
    u16, low_attrb, set_low_attrb: 11, 2;
    //get_res0, set_res0: 12, 12; -- only needed for >4KB pages
    u64, address, set_address: 47, 12;
    res0, set_res0: 49, 48;
    high_attrb, set_high_attrb: 63, 50;
}

impl PageTableEntry {
    /// Create a PageTableEntry pointing to a table in the next level in the page tree
    pub fn table_desc(table_ptr: PhysicalAddress) -> PageTableEntry {
        let mut e = PageTableEntry(0);
        e.set_valid(true);
        e.set_type(true);
        // first 12 bits are zero because the table must be page aligned??
        e.set_address(table_ptr.0 as u64 >> 12);
        e
    }

    /// Create a PageTableEntry describing a block output address
    pub fn block_entry(base_address: PhysicalAddress, level: u8) -> PageTableEntry {
        let mut e = PageTableEntry(0);
        e.set_valid(true);
        e.set_type(false);
        e.set_address(
            ((base_address.0 as u64)
                & (match level {
                    // zero out the RES0 bits
                    1 => 0xffff_ffff_f000_0000,
                    2 => 0xffff_ffff_fff0_0000,
                    _ => panic!("invalid page level {}", level),
                }))
                >> 12,
        );
        e
    }

    /// Create a PageTableEntry describing a page output address
    pub fn page_entry(base_address: PhysicalAddress) -> PageTableEntry {
        let mut e = PageTableEntry(0);
        e.set_valid(true);
        e.set_type(true);
        // first 12 bits are zero because the page must be page aligned??
        e.set_address(base_address.0 as u64 >> 12);
        e
    }

    pub fn invalid() -> PageTableEntry {
        PageTableEntry(0)
    }

    /// Return a reference to the next level table that this entry refers to, or None if the entry
    /// is un-allocated or otherwise does not refer to a level table
    /// TODO: may still return a Some(...) if the entry is actually a page descriptor!
    fn table_ref(&self, lvl: u8) -> Option<*mut LevelTable> {
        // TODO: this function is quite complex, because we need to figure out the address of the
        // table in the current VIRTUAL memory space but we only have the PHYSICAL base address.
        // One potential solution is to identity map (by necessity in the kernel's low address range)
        // all page tables, so that the physical and virtual addresses are always the same. Perhaps
        // that is jank, but it is *really* convenient. Potentially not so great for the TLB though?

        if lvl >= 3 {
            return None;
        }
        // this is safe, because at any level < 3 where table_phy_addr() is Some will have a pointer to a valid LevelTable
        self.table_phy_addr()
            .map(|PhysicalAddress(a)| a as *mut LevelTable)
    }

    /// WARN: may still return a Some(...) if the entry is actually a page descriptor! Call table_ref() first!
    fn table_phy_addr(&self) -> Option<PhysicalAddress> {
        if !self.valid() || !self.get_type() {
            return None; // invalid or block entry
        }
        Some(PhysicalAddress((self.address() as usize) << 12))
    }

    fn base_address(&self, lvl: u8) -> Option<PhysicalAddress> {
        if !self.valid() || (lvl < 3 && self.get_type()) || (lvl >= 3 && !self.get_type()) {
            return None; // invalid or table entry
        }
        Some(PhysicalAddress(self.address() as usize))
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
#[derive(Debug)]
pub struct PageTable {
    /// true => this page table is for virtual addresses of prefix 0xffff, false => prefix must be 0x0000
    high_addresses: bool,
    level0_table: *mut LevelTable,
    level0_phy_addr: PhysicalAddress,
}

#[derive(Debug)]
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
    Memory(MemoryError),
}

impl PageTable {
    pub fn empty(high_addresses: bool) -> Result<PageTable, MemoryError> {
        let level0_phy_addr = Self::allocate_table()?;
        Ok(PageTable {
            high_addresses,
            level0_table: level0_phy_addr.0 as *mut LevelTable,
            level0_phy_addr,
        })
    }

    pub fn identity(
        high_addresses: bool,
        start_addr: PhysicalAddress,
        page_count: usize,
    ) -> Result<PageTable, MapError> {
        let mut p = PageTable::empty(high_addresses).map_err(MapError::Memory)?;
        p.map_range(start_addr, VirtualAddress(start_addr.0), page_count, true)?;
        Ok(p)
    }

    pub unsafe fn activate(&self) {
        if !self.high_addresses {
            core::arch::asm!("msr TBR0_EL1, {addr}",
                addr = in(reg) self.level0_phy_addr.0)
        } else {
            core::arch::asm!("msr TBR1_EL1, {addr}",
                addr = in(reg) self.level0_phy_addr.0)
        }
        core::arch::asm!("isb");
    }

    fn allocate_table() -> Result<PhysicalAddress, MemoryError> {
        let new_page_phy_addr = physical_memory_allocator().alloc()?;
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
                for (lvl, i) in [i0, i1, i2, i3].into_iter().enumerate() {
                    let tr = unsafe { table.as_ref().expect("table ptr is valid") };
                    if let Some(existing_table) = tr[i].table_ref(lvl as u8) {
                        table = existing_table;
                    } else if tr[i].valid() {
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
            log::trace!("mapping page {page_start}=[{tag:x}:{i0:x}:{i1:x}:{i2:x}:{i3:x}:{po:x}], {} pages left", page_count - page_index);
            let mut table = self.level0_table;
            // TODO: surely there is a clean way to generate this slice with iterators?
            for (lvl, i, can_allocate_large_block, large_block_size) in [
                (0, i0, false, 0),
                (1, i1, i2 == 0 && i3 == 0, 512 * 512), // 1GiB in pages
                (2, i2, i3 == 0, 512),                  // 2MiB in pages
            ] {
                let tr = unsafe { table.as_mut().expect("table ptr is valid") };
                if let Some(existing_table) = tr[i].table_ref(lvl) {
                    table = existing_table;
                } else {
                    assert!(!tr[i].valid());
                    if can_allocate_large_block && (page_count - page_index) >= large_block_size {
                        tr[i] = PageTableEntry::block_entry(
                            PhysicalAddress(phy_start.0 + page_index * PAGE_SIZE),
                            lvl,
                        );
                        page_index += large_block_size;
                        continue 'top;
                    } else {
                        tr[i] = PageTableEntry::table_desc(
                            Self::allocate_table().map_err(MapError::Memory)?,
                        );
                        table = tr[i].table_ref(lvl).unwrap();
                    }
                }
            }

            unsafe {
                table.as_mut().expect("final table ptr is valid")[i3] = PageTableEntry::page_entry(
                    PhysicalAddress(phy_start.0 + page_index * PAGE_SIZE),
                );
            }
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
            for (lvl, i, block_size) in [
                (0, i0, 0),
                (1, i1, 512 * 512), // 1GiB in pages
                (2, i2, 512),       // 2MiB in pages
                (3, i3, 1),
            ] {
                let tr = unsafe { table.as_mut().expect("table ptr is valid") };
                if let Some(existing_table) = tr[i].table_ref(lvl) {
                    table = existing_table;
                } else {
                    assert_ne!(block_size, 0);
                    tr[i] = PageTableEntry::invalid();
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
        let (tag, i0, i1, i2, i3, po) = virt.to_parts();

        match (self.high_addresses, tag) {
            (true, 0xffff) | (false, 0x0) => {}
            _ => return None,
        }
        let mut table = self.level0_table;
        // TODO: surely there is a clean way to generate this slice with iterators?
        for (lvl, i, block_size) in [
            (0, i0, 0),
            (1, i1, 512 * 512), // 1GiB in pages
            (2, i2, 512),       // 2MiB in pages
            (3, i3, 1),
        ] {
            let tr = unsafe { table.as_ref().expect("table ptr is valid") };
            if let Some(existing_table) = tr[i].table_ref(lvl) {
                table = existing_table;
            } else {
                return tr[i]
                    .base_address(lvl)
                    .map(|PhysicalAddress(addr)| PhysicalAddress(addr + po));
            }
        }

        unreachable!("table ref in level 3 table")
    }

    pub fn virtual_address_of(&self, phy: PhysicalAddress) -> Option<VirtualAddress> {
        todo!()
    }
}

impl Drop for PageTable {
    fn drop(&mut self) {
        // Does not drop actual pages!
        fn drop_table(lvl: u8, table: *mut LevelTable, table_phy_addr: PhysicalAddress) {
            for entry in unsafe { &table.as_ref().expect("table ptr is valid").entries } {
                if let Some(table) = entry.table_ref(lvl) {
                    // we know that entry.table_phy_addr() will actually point to a table because we've already called table_ref()
                    drop_table(lvl + 1, table, entry.table_phy_addr().unwrap());
                }
            }
            physical_memory_allocator().free(table_phy_addr)
        }
        drop_table(0, self.level0_table, self.level0_phy_addr);
    }
}

pub unsafe fn enable_mmu() {
    log::debug!("enabling MMU!");
    core::arch::asm!(
        "mrs {tmp}, SCTLR_EL1", //read existing SCTLR_EL1 configuration
        "orr {tmp}, {tmp}, #1", //set MMU enable bit
        "msr SCTLR_EL1, {tmp}", //write SCTLR_EL1 back
        "isb",                  //force changes to get picked up on the next instruction
        tmp = out(reg) _
    )
}

bitfield! {
    pub struct TranslationControlReg(u64);
    u8;
    pub top_byte_ignore1, set_top_byte_ignore1: 38;
    pub top_byte_ignore0, set_top_byte_ignore0: 37;
    pub asid_size, set_asid_size: 36;
    pub ipas, set_ipas: 34, 32;

    pub granule_size1, set_granule_size1: 31, 30;
    pub shareability1, set_shareability1: 29, 28;
    pub outer_cacheablity1, set_outer_cacheablity1: 27, 26;
    pub inner_cacheablity1, set_inner_cacheablity1: 27, 26;
    pub walk_on_miss1, set_walk_on_miss1: 23;
    pub a1, set_a1: 22;
    pub size_offset1, set_size_offset1: 21, 16;

    pub granule_size0, set_granule_size0: 15, 14;
    pub shareability0, set_shareability0: 13, 12;
    pub outer_cacheablity0, set_outer_cacheablity0: 11, 10;
    pub inner_cacheablity0, set_inner_cacheablity0: 9, 8;
    pub walk_on_miss0, set_walk_on_miss0: 7;
    pub size_offset0, set_size_offset0: 5, 0;
}

pub unsafe fn get_tcr() -> TranslationControlReg {
    let mut tcr = TranslationControlReg(0);
    core::arch::asm!(
        "mrs {val}, TCR_EL1",
        val = out(reg) tcr.0
    );
    tcr
}

pub unsafe fn set_tcr(new_tcr: TranslationControlReg) {
    core::arch::asm!(
        "msr TCR_EL1, {val}",
        "isb",
        val = in(reg) new_tcr.0
    )
}
