#![allow(unused)]
use core::{cell::OnceCell, ops::Range};

use bitfield::bitfield;
use spin::Mutex;

use super::{
    physical_memory_allocator, MemoryError, PhysicalAddress, PhysicalMemoryAllocator,
    VirtualAddress, PAGE_SIZE,
};

// defined by start.S
extern "C" {
    static mut _kernel_page_table_root: u8;
}

bitfield! {
    pub struct PageTableEntry(u64);
    u64;
    high_attrb, set_high_attrb: 63, 50;
    res0, set_res0: 49, 48;
    address, set_address: 47, 12;
    //get_res0, set_res0: 12, 12; -- only needed for >4KB pages
    // u16, low_attrb, set_low_attrb: 11, 2;
    ng, set_ng: 11;
    af, set_af: 10;
    u8, sh, set_sh: 9, 8;
    ap_read_only, set_ap_read_only: 7;
    ap_el0_access, set_ap_el0_access: 6;
    ns, set_ns: 5;
    u8, attr_index, set_attr_index: 4, 2;
    get_type, set_type: 1;
    valid, set_valid: 0;
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
        e.set_af(true);
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
        e.set_af(true);
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
        // TODO: for now we are assuming that memory is identity mapped starting at
        // 0xffff_0000_0000_0000virtual = 0x0physical

        if lvl >= 3 {
            return None;
        }
        // this is safe, because at any level < 3 where table_phy_addr() is Some will have a pointer to a valid LevelTable -- so long at the identity mapping is set up correctly!
        self.table_phy_addr()
            .map(|a| unsafe { a.to_virtual_canonical().as_ptr() })
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

impl core::fmt::Debug for PageTableEntry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.valid() {
            write!(
                f,
                "{{{} ({:b}|{:b}.AF={},RO={},E0={}) @ 0x{:x}}}",
                if self.get_type() { "E" } else { "B" },
                self.high_attrb(),
                self.res0(),
                if self.af() { "1" } else { "0" },
                if self.ap_read_only() { "1" } else { "0" },
                if self.ap_el0_access() { "1" } else { "0" },
                self.address()
            )
        } else {
            write!(f, "{{invalid}}")
        }
    }
}

#[derive(Default)]
pub struct PageTableEntryOptions {
    pub read_only: bool,
    pub el0_access: bool,
}

impl PageTableEntryOptions {
    pub fn apply(&self, mut entry: PageTableEntry) -> PageTableEntry {
        entry.set_ap_read_only(self.read_only);
        entry.set_ap_el0_access(self.el0_access);
        entry
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
pub struct PageTable {
    /// true => this page table is for virtual addresses of prefix 0xffff, false => prefix must be 0x0000
    high_addresses: bool,
    pub asid: u16,
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

impl core::fmt::Debug for PageTable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "PageTable[{:x}]@{} ({}) [\n",
            self.asid,
            self.level0_phy_addr,
            if self.high_addresses { "H" } else { "L" }
        )?;
        fn print_table(
            f: &mut core::fmt::Formatter<'_>,
            lvl: u8,
            table: *mut LevelTable,
            table_phy_addr: PhysicalAddress,
        ) -> core::fmt::Result {
            let mut entries = unsafe { &table.as_ref().expect("table ptr is valid").entries }
                .iter()
                .peekable();
            let mut index = 0;
            while let Some(entry) = entries.next() {
                for _ in 0..lvl {
                    write!(f, "\t")?;
                }
                write!(f, "0x{index:x} {entry:?}")?;
                if let Some(table) = entry.table_ref(lvl) {
                    write!(f, " [\n")?;
                    // we know that entry.table_phy_addr() will actually point to a table because we've already called table_ref()
                    print_table(f, lvl + 1, table, entry.table_phy_addr().unwrap())?;
                    for _ in 0..lvl {
                        write!(f, "\t")?;
                    }
                    write!(f, "]")?;
                }
                if entries.peek().is_some_and(|ne| ne.0 == entry.0) {
                    let mut n = 0;
                    while entries.peek().is_some_and(|ne| ne.0 == entry.0) {
                        entries.next();
                        n += 1;
                    }
                    write!(f, "x{n}...")?;
                }
                write!(f, ",\n")?;
                index += 1;
            }
            Ok(())
        }
        print_table(f, 0, self.level0_table, self.level0_phy_addr)?;
        write!(f, "]")
    }
}

impl PageTable {
    pub unsafe fn at_address(high_addresses: bool, addr: PhysicalAddress, asid: u16) -> PageTable {
        PageTable {
            high_addresses,
            asid,
            level0_table: unsafe { addr.to_virtual_canonical().as_ptr() },
            level0_phy_addr: addr,
        }
    }

    // TODO: how do we synchronize access to this table if there is more than one reference to it?
    unsafe fn kernel_table() -> PageTable {
        let level0_table = core::mem::transmute(&mut _kernel_page_table_root);

        PageTable {
            high_addresses: true,
            asid: 0,
            level0_table,
            level0_phy_addr: VirtualAddress::from(level0_table).to_physical_canonical(),
        }
    }

    pub fn empty(high_addresses: bool, asid: u16) -> Result<PageTable, MemoryError> {
        let level0_phy_addr = Self::allocate_table()?;
        Ok(PageTable {
            high_addresses,
            asid,
            level0_table: level0_phy_addr.0 as *mut LevelTable,
            level0_phy_addr,
        })
    }

    pub fn identity(
        high_addresses: bool,
        start_addr: PhysicalAddress,
        page_count: usize,
        asid: u16,
        options: &PageTableEntryOptions,
    ) -> Result<PageTable, MapError> {
        let mut p = PageTable::empty(high_addresses, asid).map_err(MapError::Memory)?;
        p.map_range(
            start_addr,
            VirtualAddress(start_addr.0),
            page_count,
            true,
            &options,
        )?;
        Ok(p)
    }

    pub unsafe fn activate(&self) {
        log::debug!(
            "activating table @ {:x}, ASID {:x}",
            self.level0_phy_addr.0,
            self.asid
        );
        // CnP bit is set to zero by assumption rn, see D17.2.144
        let v = ((self.asid as usize) << 48) | self.level0_phy_addr.0;
        log::debug!(
            "TTBR{}_EL1 ??? {:16x}",
            if self.high_addresses { 1 } else { 0 },
            v
        );
        if !self.high_addresses {
            core::arch::asm!("msr TTBR0_EL1, {addr}",
                addr = in(reg) v)
        } else {
            core::arch::asm!("msr TTBR1_EL1, {addr}",
                addr = in(reg) v)
        }
        // core::arch::asm!("isb");
    }

    fn allocate_table() -> Result<PhysicalAddress, MemoryError> {
        let new_page_phy_addr = physical_memory_allocator().alloc()?;
        log::trace!("allocating new page table at {}", new_page_phy_addr);
        unsafe {
            // WARN: Assume that memory is identity mapped in the high addresses!
            let table_ptr: *mut LevelTable = new_page_phy_addr.to_virtual_canonical().as_ptr();
            core::ptr::write_bytes(table_ptr, 0, 1);
        }
        Ok(new_page_phy_addr)
    }

    pub fn map_range(
        &mut self,
        phy_start: PhysicalAddress,
        virt_start: VirtualAddress,
        page_count: usize,
        overwrite: bool,
        options: &PageTableEntryOptions,
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
                        tr[i] = options.apply(PageTableEntry::block_entry(
                            PhysicalAddress(phy_start.0 + page_index * PAGE_SIZE),
                            lvl,
                        ));
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
                table.as_mut().expect("final table ptr is valid")[i3] =
                    options.apply(PageTableEntry::page_entry(PhysicalAddress(
                        phy_start.0 + page_index * PAGE_SIZE,
                    )));
            }
            page_index += 1;
        }

        Ok(())
    }

    pub fn unmap_range(&mut self, virt_start: VirtualAddress, page_count: usize) {
        let mut page_index = 0;
        'top: while page_index < page_count {
            let page_addr = VirtualAddress(virt_start.0 + page_index * PAGE_SIZE);
            log::trace!("{page_addr}");
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
        log::trace!("[{tag:x}:{i0:x}:{i1:x}:{i2:x}:{i3:x}:{po:x}]");

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
            log::debug!("lvl={lvl:x}, i={i}, block_size={block_size}");
            let tr = unsafe { table.as_ref().expect("table ptr is valid") };
            log::debug!("{table:?}");
            if let Some(existing_table) = tr[i].table_ref(lvl) {
                table = existing_table;
            } else {
                log::debug!("{:x}", tr[i].0);
                //TODO: this isn't correct if this is a block because it should
                //include bits from i1/i2/i3 in the page/block offset
                return tr[i]
                    .base_address(lvl)
                    .map(|PhysicalAddress(addr)| PhysicalAddress(addr << 12 + po));
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
        // WARN: Does not drop actual pages!
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

pub unsafe fn disable_mmu() {
    log::debug!("disabling MMU!");
    core::arch::asm!(
        "isb",
        "mrs {tmp}, SCTLR_EL1", //read existing SCTLR_EL1 configuration
        "bic {tmp}, {tmp}, #1", //clear MMU enable bit
        "msr SCTLR_EL1, {tmp}", //write SCTLR_EL1 back
        "isb",                  //force changes to get picked up on the next instruction
        tmp = out(reg) _
    )
}

pub unsafe fn enable_mmu() {
    log::debug!("enabling MMU!");
    core::arch::asm!(
        "isb",
        "mrs {tmp}, SCTLR_EL1", //read existing SCTLR_EL1 configuration
        "orr {tmp}, {tmp}, #1", //set MMU enable bit
        "msr SCTLR_EL1, {tmp}", //write SCTLR_EL1 back
        "isb",                  //force changes to get picked up on the next instruction
        tmp = out(reg) _
    )
}

bitfield! {
    pub struct TranslationControlReg(u64);
    impl Debug;
    u8;
    pub ds, set_ds: 59;
    pub tcma1, set_tcma1: 58;
    pub tcma0, set_tcma0: 57;
    pub e0pd1, set_e0pd1: 56;
    pub e0pd0, set_e0pd0: 55;
    pub nfd1, set_nfd1: 54;
    pub nfd0, set_nfd0: 53;
    pub tbid1, set_tbid1: 52;
    pub tbid0, set_tbid0: 51;
    pub hardware_use_enable, set_hardware_use_enable: 50, 43;
    pub hpd1, set_hpd1: 42;
    pub hpd0, set_hpd0: 41;
    pub hd, set_hd: 40;
    pub ha, set_ha: 39;
    pub top_byte_ignore1, set_top_byte_ignore1: 38;
    pub top_byte_ignore0, set_top_byte_ignore0: 37;
    pub asid_size, set_asid_size: 36;
    pub ipas, set_ipas: 34, 32;

    pub granule_size1, set_granule_size1: 31, 30;
    pub shareability1, set_shareability1: 29, 28;
    pub outer_cacheablity1, set_outer_cacheablity1: 27, 26;
    pub inner_cacheablity1, set_inner_cacheablity1: 27, 26;
    pub epd1, set_epd1: 23;
    pub a1, set_a1: 22;
    pub size_offset1, set_size_offset1: 21, 16;

    pub granule_size0, set_granule_size0: 15, 14;
    pub shareability0, set_shareability0: 13, 12;
    pub outer_cacheablity0, set_outer_cacheablity0: 11, 10;
    pub inner_cacheablity0, set_inner_cacheablity0: 9, 8;
    pub epd0, set_epd0: 7;
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
    log::trace!("setting TCR to {:x}", new_tcr.0);
    core::arch::asm!(
        "msr TCR_EL1, {val}",
        // "isb",
        val = in(reg) new_tcr.0
    )
}

pub unsafe fn flush_tlb_total() {
    core::arch::asm!(
        "DSB ISHST",    // ensure writes to tables have completed
        "TLBI VMALLE1", // flush entire TLB. The programming guide uses the 'ALLE1'
        // variant, which causes a fault in QEMU with EC=0, but
        // https://forum.osdev.org/viewtopic.php?t=36412&p=303237
        // suggests using VMALLE1 instead, which appears to work
        "DSB ISH", // ensure that flush has completed
        "ISB",     // make sure next instruction is fetched with changes
    )
}

pub unsafe fn flush_tlb_for_asid(asid: u16) {
    core::arch::asm!(
        "DSB ISHST", // ensure writes to tables have completed
        "TLBI ASIDE1, {asid:x}", // flush TLB entries associated with ASID
        "DSB ISH", // ensure that flush has completed
        "ISB", // make sure next instruction is fetched with changes
        asid = in(reg) asid
    )
}

static mut KERNEL_TABLE: OnceCell<Mutex<PageTable>> = OnceCell::new();

pub unsafe fn init_kernel_page_table() {
    KERNEL_TABLE
        .set(Mutex::new(PageTable::kernel_table()))
        .expect("init kernel page table once");
}

pub fn kernel_table() -> spin::MutexGuard<'static, PageTable> {
    unsafe {
        KERNEL_TABLE
            .get()
            .as_ref()
            .expect("kernel page table initialized")
            .lock()
    }
}
