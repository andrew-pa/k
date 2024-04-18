//! Page table structure and MMU interface.
#![allow(unused)]
use core::{cell::OnceCell, ops::Range, ptr::NonNull, sync::atomic::AtomicBool};

use bitfield::bitfield;
use smallvec::SmallVec;
use snafu::{ensure, OptionExt, ResultExt, Snafu};
use spin::Mutex;

use crate::exception::InterruptGuard;

use super::{
    physical_memory_allocator, MapSnafu, MemoryError, PhysicalAddress, PhysicalMemoryAllocator,
    VirtualAddress, PAGE_SIZE,
};

// defined by start.S
extern "C" {
    static mut _kernel_page_table_root: u8;
}

bitfield! {
    /// An entry of a page table.
    struct PageTableEntry(u64);
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

    /// Create a PageTableEntry describing a block output address.
    /// Returns None if the address is not properly aligned to be allocated as a block at this level.
    pub fn block_entry(base_address: PhysicalAddress, level: u8) -> Option<PageTableEntry> {
        match level {
            1 => {
                if (base_address.0 & 0xff_ffff) != 0 {
                    return None;
                }
            }
            2 => {
                if (base_address.0 & 0xf_ffff) != 0 {
                    return None;
                }
            }
            _ => return None,
        }
        let mut e = PageTableEntry(0);
        e.set_valid(true);
        e.set_type(false);
        e.set_af(true);
        e.set_address((base_address.0 as u64) >> 12);
        Some(e)
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
    fn table_ref(&self, lvl: u8) -> Option<NonNull<LevelTable>> {
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
            .and_then(NonNull::new)
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
        Some(PhysicalAddress((self.address() as usize) << 12))
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

/// Options for creating a page table entry.
#[derive(Default)]
pub struct PageTableEntryOptions {
    /// True if the page should be read-only, false if read-write.
    pub read_only: bool,
    /// The page should be accessable to user-space (EL0).
    pub el0_access: bool,
}

impl PageTableEntryOptions {
    fn apply(&self, mut entry: PageTableEntry) -> PageTableEntry {
        entry.set_ap_read_only(self.read_only);
        entry.set_ap_el0_access(self.el0_access);
        entry
    }
}

/// Errors arising from page table mapping operations.
#[derive(Debug, Snafu)]
pub enum MapError {
    #[snafu(display("Range already mapped. start={virt_start}, page_count={page_count}, collision address={collision}"))]
    RangeAlreadyMapped {
        virt_start: VirtualAddress,
        page_count: usize,
        collision: VirtualAddress,
    },
    #[snafu(display("Bad virtual address {virt_start}: {reason}"))]
    MapRangeBadVirtualBase {
        reason: &'static str,
        virt_start: VirtualAddress,
    },
    #[snafu(display("Insufficent map space for {page_count}, start={virt_start}"))]
    InsufficentMapSpace {
        page_count: usize,
        virt_start: VirtualAddress,
    },
    InvalidTag,
    #[snafu(display("Copy from unmapped region. last valid address={last_valid:?}, {remaining_bytes} bytes remaining in copy"))]
    CopyFromUnmapped {
        last_valid: Option<VirtualAddress>,
        remaining_bytes: usize,
    },
}

type LevelTable = [PageTableEntry; 512];

/// A page table tree in memory.
///
/// This structure does not require heap allocation, but does expect that it can allocate the
/// memory for the tables in the identity mapped region of main memory.
///
/// This structure is internally mutable and synchronized. The root table address however is fixed.
// WARN: TODO: Currently assumes that physical memory is identity mapped in the 0x0000 prefix!
pub struct PageTable {
    /// true => this page table is for virtual addresses of prefix 0xffff, false => prefix must be 0x0000
    high_addresses: bool,
    modified: AtomicBool,
    /// The address space ID (ASID) for this page table, for use in caching.
    pub asid: u16,
    level0_table: Mutex<NonNull<LevelTable>>,
    level0_phy_addr: PhysicalAddress,
}

/// # Safety
/// [PageTable] is internally synchronized.
unsafe impl Send for PageTable {}
/// # Safety
/// [PageTable] is internally synchronized.
unsafe impl Sync for PageTable {}

impl core::fmt::Debug for PageTable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(
            f,
            "PageTable[{:x}]@{} ({}) [",
            self.asid,
            self.level0_phy_addr,
            if self.high_addresses { "H" } else { "L" }
        )?;
        fn print_table(
            f: &mut core::fmt::Formatter<'_>,
            lvl: u8,
            table: NonNull<LevelTable>,
            table_phy_addr: PhysicalAddress,
        ) -> core::fmt::Result {
            let mut entries = unsafe { table.as_ref() }.iter().peekable();
            let mut index = 0;
            while let Some(entry) = entries.next() {
                for _ in 0..lvl {
                    write!(f, "\t")?;
                }
                write!(f, "0x{index:x} {entry:?}")?;
                if let Some(table) = entry.table_ref(lvl) {
                    writeln!(f, " [")?;
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
                    index += n;
                }
                writeln!(f, ",")?;
                index += 1;
            }
            Ok(())
        }
        print_table(f, 0, *self.level0_table.lock(), self.level0_phy_addr)?;
        write!(f, "]")
    }
}

impl PageTable {
    /// Create the [`PageTable`] instance that represents the kernel's page table that is
    /// originally constructed in `start.S` at `_kernel_page_table_root`.
    ///
    /// # Safety
    /// This function should only be called once to prevent aliased references to the same table.
    /// This function also assumes that `start.S` did its job correctly.
    // TODO: how do we synchronize access to this table if there is more than one reference to it?
    unsafe fn kernel_table() -> PageTable {
        let level0_table: *mut LevelTable =
            core::mem::transmute(core::ptr::addr_of_mut!(_kernel_page_table_root));

        PageTable {
            high_addresses: true,
            asid: 0,
            level0_table: Mutex::new(
                NonNull::new(level0_table).expect("kernel page table pointer is non-null"),
            ),
            level0_phy_addr: VirtualAddress::from(level0_table).to_physical_canonical(),
            modified: AtomicBool::new(false),
        }
    }

    /// Create a new, empty page table.
    ///
    /// If `high_addresses` is true, then the table will map addresses starting with `0xffff_...`,
    /// otherwise the addresses will start with all zeros.
    pub fn empty(high_addresses: bool, asid: u16) -> Result<PageTable, MemoryError> {
        let level0_phy_addr = Self::allocate_table()?;
        Ok(PageTable {
            high_addresses,
            asid,
            level0_table: unsafe {
                Mutex::new(
                    NonNull::new(level0_phy_addr.to_virtual_canonical().as_ptr())
                        .expect("allocator returns non-null pointer"),
                )
            },
            level0_phy_addr,
            modified: AtomicBool::new(false),
        })
    }

    /// Create a page table tree that produces an identity mapping starting at `start_addr` of
    /// length `page_count`.
    ///
    /// If `high_addresses` is true, then the table will map addresses starting with `0xffff_...`,
    /// otherwise the addresses will start with all zeros.
    pub fn identity(
        high_addresses: bool,
        start_addr: PhysicalAddress,
        page_count: usize,
        asid: u16,
        options: &PageTableEntryOptions,
    ) -> Result<PageTable, MemoryError> {
        let mut p = PageTable::empty(high_addresses, asid)?;
        p.map_range(
            start_addr,
            VirtualAddress(start_addr.0),
            page_count,
            true,
            options,
        )?;
        Ok(p)
    }

    /// Activate this page table as the current one being used to map virtual addresses.
    ///
    /// # Safety
    /// It is up to the caller to make sure that we can continue to execute after this function is
    /// called, i.e. that the instruction pointer will still be mapped to the expected place in the program.
    /// This function writes system registers.
    pub unsafe fn activate(&self) {
        log::trace!(
            "activating table @ {}, ASID {:x}",
            self.level0_phy_addr,
            self.asid
        );
        // CnP bit is set to zero by assumption rn, see D17.2.144
        // TODO: this seems rather incorrect, since it basically shifts the page table address down
        // by one bit because it uses the (ostensibly) ending zero in the address as the CnP bit.
        // To do this correctly we need to know the value of TCR_EL1.TxSZ + granule size, and then
        // shift the page table address up accordingly
        let v = ((self.asid as usize) << 48) | self.level0_phy_addr.0;
        // log::trace!(
        //     "TTBR{}_EL1 ← {:16x}",
        //     if self.high_addresses { 1 } else { 0 },
        //     v
        // );
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

    /// Map a range of virtual addresses starting at `virt_start` to the physical memory starting
    /// at `phy_start`. The `options` will be applied to all the pages in the region.
    pub fn map_range(
        &self,
        phy_start: PhysicalAddress,
        virt_start: VirtualAddress,
        page_count: usize,
        overwrite: bool,
        options: &PageTableEntryOptions,
    ) -> Result<(), MemoryError> {
        // determine how big page_count is relative to the size of the higher level page table blocks and figure out how many new tables we'll need, if any
        let num_l3_tables = page_count.div_ceil(512);
        let num_l2_tables = num_l3_tables.div_ceil(512);
        let num_l1_tables = num_l2_tables.div_ceil(512);

        // find spot in page table where virt_start is
        let (tag, lv0_start, lv1_start, lv2_start, lv3_start, page_offset) = virt_start.to_parts();
        if page_offset != 0 {
            return Err(MemoryError::Map {
                source: MapError::MapRangeBadVirtualBase {
                    reason: "starting virtual address for mapping range must be at a page boundary",
                    virt_start,
                },
            });
        }
        match (self.high_addresses, tag) {
            (true, 0xffff) | (false, 0x0) => {}
            _ => return Err(MemoryError::Map {
                source: MapError::MapRangeBadVirtualBase {
                    reason:
                        "starting virtual address for mapping range must match tag for page table",
                    virt_start,
                },
            }),
        }

        if !overwrite {
            todo!();
        }

        // allocate new page tables and add required entries
        let mut page_index = 0;
        // prevent deadlocks with interrupt handlers
        let _interrupt_guard = InterruptGuard::disable_interrupts_until_drop();
        // lock the table until we're finished manipulating it
        let level0_table = self.level0_table.lock();
        'top: while page_index < page_count {
            let page_start = VirtualAddress(virt_start.0 + page_index * PAGE_SIZE);
            let (tag, i0, i1, i2, i3, po) = page_start.to_parts();
            // log::trace!("mapping page {page_start}=[{tag:x}:{i0:x}:{i1:x}:{i2:x}:{i3:x}:{po:x}], {} pages left", page_count - page_index);
            let mut table = *level0_table;
            // TODO: surely there is a clean way to generate this slice with iterators?
            for (lvl, i, can_allocate_large_block, large_block_size) in [
                (0, i0, false, 0),
                (1, i1, i2 == 0 && i3 == 0, 512 * 512), // 1GiB in pages
                (2, i2, i3 == 0, 512),                  // 2MiB in pages
            ] {
                let tr = unsafe { table.as_mut() };
                if let Some(existing_table) = tr[i].table_ref(lvl) {
                    table = existing_table;
                } else {
                    assert!(!tr[i].valid());
                    if can_allocate_large_block && (page_count - page_index) >= large_block_size {
                        if let Some(e) = PageTableEntry::block_entry(
                            PhysicalAddress(phy_start.0 + page_index * PAGE_SIZE),
                            lvl,
                        ) {
                            tr[i] = options.apply(e);
                            page_index += large_block_size;
                            continue 'top;
                        }
                    }

                    tr[i] = PageTableEntry::table_desc(Self::allocate_table()?);
                    table = tr[i].table_ref(lvl).unwrap();
                }
            }

            unsafe {
                table.as_mut()[i3] = options.apply(PageTableEntry::page_entry(PhysicalAddress(
                    phy_start.0 + page_index * PAGE_SIZE,
                )));
            }
            page_index += 1;
        }

        self.modified
            .store(true, core::sync::atomic::Ordering::Relaxed);

        Ok(())
    }

    /// Clear the page table of a mapping starting at `virt_start`.
    ///
    /// TODO: This does not yet free the memory used up by now empty page tables.
    pub fn unmap_range(&self, virt_start: VirtualAddress, page_count: usize) {
        self.unmap_range_custom(virt_start, page_count, |_| ())
    }

    /// Clear the page table of a mapping starting at `virt_start`, calling `process_region` for
    /// each unmapped region before it is unmapped.
    ///
    /// TODO: This does not yet free the memory used up by now empty page tables.
    pub fn unmap_range_custom(
        &self,
        virt_start: VirtualAddress,
        page_count: usize,
        mut process_region: impl FnMut(MappedRegion),
    ) {
        // prevent deadlocks with interrupt handlers
        let _interrupt_guard = InterruptGuard::disable_interrupts_until_drop();
        // lock the table until we're finished manipulating it
        let level0_table = self.level0_table.lock();
        let mut page_index = 0;
        'top: while page_index < page_count {
            let page_addr = VirtualAddress(virt_start.0 + page_index * PAGE_SIZE);
            let (tag, i0, i1, i2, i3, po) = page_addr.to_parts();
            let mut table = *level0_table;
            // TODO: surely there is a clean way to generate this slice with iterators?
            for (lvl, i, block_size) in [
                (0, i0, 0),
                (1, i1, 512 * 512), // 1GiB in pages
                (2, i2, 512),       // 2MiB in pages
                (3, i3, 1),
            ] {
                let tr = unsafe { table.as_mut() };
                if let Some(existing_table) = tr[i].table_ref(lvl) {
                    table = existing_table;
                } else {
                    assert_ne!(block_size, 0);
                    if let Some(base_phys_addr) = tr[i].base_address(lvl) {
                        process_region(MappedRegion {
                            base_virt_addr: page_addr,
                            base_phys_addr,
                            page_count: block_size,
                        });
                    }
                    tr[i] = PageTableEntry::invalid();
                    page_index += block_size;
                    break;
                }
            }
        }
        self.modified
            .store(true, core::sync::atomic::Ordering::Relaxed);
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

        // prevent deadlocks with interrupt handlers
        let _interrupt_guard = InterruptGuard::disable_interrupts_until_drop();
        // lock the table until we're finished reading it
        let level0_table = self.level0_table.lock();

        let mut table = *level0_table;
        // TODO: surely there is a clean way to generate this slice with iterators?
        for (lvl, i, block_size) in [
            (0, i0, 0),
            (1, i1, 512 * 512), // 1GiB in pages
            (2, i2, 512),       // 2MiB in pages
            (3, i3, 1),
        ] {
            log::debug!("lvl={lvl:x}, i={i}, block_size={block_size}");
            let tr = unsafe { table.as_ref() };
            log::debug!("{table:?}");
            if let Some(existing_table) = tr[i].table_ref(lvl) {
                table = existing_table;
            } else {
                log::debug!("{:x}", tr[i].0);
                //TODO: this isn't correct if this is a block because it should
                //include bits from i1/i2/i3 in the page/block offset
                return tr[i]
                    .base_address(lvl)
                    .map(|PhysicalAddress(addr)| PhysicalAddress(addr + po));
            }
        }

        unreachable!("table ref in level 3 table")
    }

    /// Copy the data residing in physical memory mapped in the range `src_start..src_start+dst.len()` in the page table to `dst`.
    /// This uses the canonical physical to virtual mapping to read each page.
    /// The whole range must be mapped, or an error will occur.
    pub fn mapped_copy_from(
        &self,
        src_start: VirtualAddress,
        dst: &mut [u8],
    ) -> Result<(), MemoryError> {
        let (tag, i0, i1, i2, i3, po) = src_start.to_parts();

        if !matches!((self.high_addresses, tag), (true, 0xffff) | (false, 0x0)) {
            return InvalidTagSnafu.fail().context(MapSnafu);
        }

        let mut i = PageTableIter::starting_at(self, [i0, i1, i2, i3]);

        let first = i
            .next()
            .context(CopyFromUnmappedSnafu {
                last_valid: None,
                remaining_bytes: dst.len(),
            })
            .context(MapSnafu)?;

        // the first region in the table before the starting address is actually a totally
        // different part of memory means that the entire region is unmapped/invalid.
        if first.base_virt_addr.add(po) != src_start {
            return Err(MemoryError::Map {
                source: MapError::CopyFromUnmapped {
                    last_valid: None,
                    remaining_bytes: dst.len(),
                },
            });
        }

        let mut offset = (first.page_count * PAGE_SIZE - po).min(dst.len());
        log::trace!(
            "first copy {}..{offset} -> 0..{offset} ({:?})",
            first.base_phys_addr,
            first
        );
        unsafe {
            dst[0..offset].copy_from_slice(core::slice::from_raw_parts(
                first.base_phys_addr.to_virtual_canonical().add(po).as_ptr(),
                offset,
            ));
        }
        let mut next_start = first.base_virt_addr.add(first.page_count * PAGE_SIZE);
        for region in i {
            log::trace!("next {region:?} (offset={offset})");
            if dst.len() - offset == 0 {
                // we copied the entire buffer so we're done
                return Ok(());
            }

            if region.base_virt_addr != next_start {
                // there was a break in the continuous virtual addresses so we hit an unmapped
                // region in the requested copy region
                break;
            }

            let len = (region.page_count * PAGE_SIZE).min(dst.len() - offset);

            let region_slice = unsafe {
                core::slice::from_raw_parts(
                    region.base_phys_addr.to_virtual_canonical().as_ptr(),
                    len,
                )
            };

            log::trace!("copy {}..{len} -> {offset}..{len}", region.base_phys_addr);
            dst[offset..offset + len].copy_from_slice(region_slice);

            next_start = region.base_virt_addr.add(region.page_count * PAGE_SIZE);
            offset += len;
        }

        Err(MemoryError::Map {
            source: MapError::CopyFromUnmapped {
                last_valid: Some(next_start),
                remaining_bytes: dst.len() - offset,
            },
        })
    }

    /// Iterate over the mapped regions in this page table.
    pub fn iter(&self) -> PageTableIter {
        PageTableIter::new(self)
    }
}

impl Drop for PageTable {
    fn drop(&mut self) {
        // WARN: Does not drop actual pages!
        fn drop_table(lvl: u8, table: NonNull<LevelTable>, table_phy_addr: PhysicalAddress) {
            for entry in unsafe { table.as_ref() } {
                if let Some(table) = entry.table_ref(lvl) {
                    // we know that entry.table_phy_addr() will actually point to a table because we've already called table_ref()
                    drop_table(lvl + 1, table, entry.table_phy_addr().unwrap());
                }
            }
            physical_memory_allocator().free(table_phy_addr)
        }
        // prevent deadlock
        let _interrupt_guard = InterruptGuard::disable_interrupts_until_drop();
        drop_table(0, *self.level0_table.lock(), self.level0_phy_addr);
    }
}

/// A mapped region in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappedRegion {
    /// The base physical address that is mapped to.
    pub base_phys_addr: PhysicalAddress,
    /// The base virtual address that is mapped from.
    pub base_virt_addr: VirtualAddress,
    /// The number of pages in this mapping.
    pub page_count: usize,
}

/// An iterator over [MappedRegion]s of a [PageTable].
pub struct PageTableIter<'a> {
    table_lock_guard: spin::MutexGuard<'a, NonNull<LevelTable>>,
    interrupt_guard: InterruptGuard,
    stack: SmallVec<[(&'a LevelTable, usize); 4]>,
    tag: usize,
}

impl<'a> PageTableIter<'a> {
    fn new(pt: &'a PageTable) -> Self {
        Self::starting_at(pt, [0, 0, 0, 0])
    }

    fn starting_at(pt: &'a PageTable, indices: [usize; 4]) -> Self {
        let interrupt_guard = InterruptGuard::disable_interrupts_until_drop();
        let table_lock_guard = pt.level0_table.lock();
        let mut s = PageTableIter {
            stack: smallvec::smallvec![(unsafe { table_lock_guard.as_ref() }, indices[0])],
            table_lock_guard,
            interrupt_guard,
            tag: if pt.high_addresses { 0xffff } else { 0x0000 },
        };
        s.follow_tree(&indices[1..]);
        s
    }

    fn current_entry(&self) -> Option<&PageTableEntry> {
        self.stack.last().and_then(|(t, i)| t.get(*i))
    }

    #[inline]
    fn lvl(&self) -> u8 {
        (self.stack.len().saturating_sub(1)) as u8
    }

    /// size of range spanned by an entry (i.e. the number of bytes mapped under this entry)
    fn current_entry_span(&self) -> usize {
        match self.lvl() {
            0 => 512 * 512 * 512 * PAGE_SIZE,
            1 => 512 * 512 * PAGE_SIZE,
            2 => 512 * PAGE_SIZE,
            3 => PAGE_SIZE,
            _ => unreachable!(),
        }
    }

    fn follow_tree(&mut self, indices: &[usize]) {
        let mut i = indices.iter();
        while let Some(p) = self
            .current_entry()
            .filter(|e| e.valid())
            .and_then(|e| e.table_ref(self.lvl()))
        {
            unsafe {
                self.stack.push((p.as_ref(), *i.next().unwrap()));
            }
        }
    }

    fn current_va_from_stack(&self) -> VirtualAddress {
        VirtualAddress::from_parts(
            self.tag,
            self.stack.first().map(|(_, i)| *i).unwrap_or(0),
            self.stack.get(1).map(|(_, i)| *i).unwrap_or(0),
            self.stack.get(2).map(|(_, i)| *i).unwrap_or(0),
            self.stack.get(3).map(|(_, i)| *i).unwrap_or(0),
            0,
        )
    }

    fn log_stack(&self) {
        // log::trace!("l0@{:?}, l1@{:?}, l2@{:?}, l3@{:?}; cva={}",
        //     self.stack.get(0).map(|(_, i)| i),
        //     self.stack.get(1).map(|(_, i)| i),
        //     self.stack.get(2).map(|(_, i)| i),
        //     self.stack.get(3).map(|(_, i)| i),
        //     self.current_va_from_stack());
    }

    fn next_entry(&mut self) {
        self.log_stack();
        let ces = self.current_entry_span();
        if let Some((ct, ci)) = self.stack.last_mut() {
            *ci += 1;
            if *ci >= ct.len() {
                self.stack.pop().unwrap();
                self.next_entry();
            }
            self.follow_tree(&[0, 0, 0]);
        }
    }

    /// move to the next valid entry that represents a mapped region
    fn move_to_next_valid_map_entry(&mut self) {
        while let Some(e) = self.current_entry() {
            if e.valid() {
                return;
            } else {
                self.next_entry();
            }
        }
    }
}

impl<'a> Iterator for PageTableIter<'a> {
    type Item = MappedRegion;

    fn next(&mut self) -> Option<Self::Item> {
        self.move_to_next_valid_map_entry();
        // inspect current entry
        let ce = self.current_entry()?;
        let res = Some(MappedRegion {
            base_phys_addr: ce.base_address(self.lvl()).expect(
                "move_to_next_valid_map_entry moved to a valid entry representing a mapped region",
            ),
            base_virt_addr: self.current_va_from_stack(),
            page_count: self.current_entry_span() / PAGE_SIZE,
        });
        self.next_entry();
        res
    }
}

/// Disable the MMU
///
/// # Safety
/// Turns off virtual memory. Must ensure this is OK in context.
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

/// Enables the MMU
///
/// # Safety
/// Turns on virtual memory. Must ensure this is OK in context.
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
    /// A value of the Translation Control Register (TCR).
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

    /// TG1
    pub granule_size1, set_granule_size1: 31, 30;
    pub shareability1, set_shareability1: 29, 28;
    pub outer_cacheablity1, set_outer_cacheablity1: 27, 26;
    pub inner_cacheablity1, set_inner_cacheablity1: 27, 26;
    pub epd1, set_epd1: 23;
    pub a1, set_a1: 22;
    pub size_offset1, set_size_offset1: 21, 16;

    /// TG0
    pub granule_size0, set_granule_size0: 15, 14;
    pub shareability0, set_shareability0: 13, 12;
    pub outer_cacheablity0, set_outer_cacheablity0: 11, 10;
    pub inner_cacheablity0, set_inner_cacheablity0: 9, 8;
    pub epd0, set_epd0: 7;
    pub size_offset0, set_size_offset0: 5, 0;
}

/// Read the TCR register value.
pub fn read_tcr() -> TranslationControlReg {
    let mut tcr = TranslationControlReg(0);
    unsafe {
        core::arch::asm!(
            "mrs {val}, TCR_EL1",
            val = out(reg) tcr.0
        );
    }
    tcr
}

/// Write a new value to the TCR register.
///
/// # Safety
/// It is up to the caller to make sure that the new TCR value is correct.
pub unsafe fn write_tcr(new_tcr: TranslationControlReg) {
    log::trace!("setting TCR to {:x}", new_tcr.0);
    core::arch::asm!(
        "msr TCR_EL1, {val}",
        // "isb",
        val = in(reg) new_tcr.0
    )
}

/// Flush the TLB for everything in EL1.
///
/// # Safety
/// It is up to the caller to make sure that the flush makes sense in context.
pub unsafe fn flush_tlb_total_el1() {
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

/// Flush the TLB for a specific ASID.
///
/// # Safety
/// It is up to the caller to make sure that the flush makes sense in context.
pub unsafe fn flush_tlb_for_asid(asid: u16) {
    core::arch::asm!(
        "DSB ISHST", // ensure writes to tables have completed
        "TLBI ASIDE1, {asid:x}", // flush TLB entries associated with ASID
        "DSB ISH", // ensure that flush has completed
        "ISB", // make sure next instruction is fetched with changes
        asid = in(reg) asid
    )
}

static mut KERNEL_TABLE: OnceCell<PageTable> = OnceCell::new();

/// Initialize the kernel page table.
pub fn init_kernel_page_table() {
    unsafe {
        KERNEL_TABLE
            .set(PageTable::kernel_table())
            .expect("init kernel page table once");
    }
}

/// A special guard type that wraps a reference [PageTable].
/// When the guard is dropped the TLB is automatically flushed if the modified flag is set.
pub struct KernelTableGuard {
    table: &'static PageTable,
}

impl core::ops::Deref for KernelTableGuard {
    type Target = PageTable;

    fn deref(&self) -> &Self::Target {
        self.table
    }
}

impl Drop for KernelTableGuard {
    fn drop(&mut self) {
        if self
            .table
            .modified
            .load(core::sync::atomic::Ordering::Acquire)
        {
            unsafe {
                flush_tlb_total_el1();
            }
        }
    }
}

/// Gain access to the kernel page table.
pub fn kernel_table() -> KernelTableGuard {
    unsafe {
        KernelTableGuard {
            table: KERNEL_TABLE.get().expect("kernel page table initialized"),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::memory::{PhysicalBuffer, PAGE_SIZE};

    use super::kernel_table;

    #[test_case]
    fn mapped_copy_small() {
        let test_data = b"mapped_copy_from test";

        let mut buf =
            PhysicalBuffer::alloc(1, &Default::default()).expect("allocate physical memory");
        buf.as_bytes_mut()[0..21].copy_from_slice(test_data);

        let mut dst = [0u8; 21];
        kernel_table()
            .mapped_copy_from(buf.virtual_address(), &mut dst)
            .expect("do copy");

        assert_eq!(&dst, test_data);
    }

    #[test_case]
    fn mapped_copy_big() {
        let mut buf =
            PhysicalBuffer::alloc_zeroed(2, &Default::default()).expect("allocate physical memory");
        for i in buf.as_bytes_mut() {
            *i = 42;
        }

        let mut dst = [0u8; 2 * PAGE_SIZE];
        kernel_table()
            .mapped_copy_from(buf.virtual_address(), &mut dst)
            .expect("do copy");

        for i in dst {
            assert_eq!(i, 42);
        }
    }

    #[test_case]
    fn mapped_copy_unaligned() {
        let mut buf =
            PhysicalBuffer::alloc_zeroed(2, &Default::default()).expect("allocate physical memory");
        for (i, b) in buf.as_bytes_mut().iter_mut().enumerate() {
            *b = (i % 256) as u8;
        }

        let mut dst = [0u8; 2 * PAGE_SIZE - 32];
        kernel_table()
            .mapped_copy_from(buf.virtual_address().add(16), &mut dst)
            .expect("do copy");

        let mut x = 16;
        for i in dst {
            assert_eq!(i, x);
            x = x.wrapping_add(1);
        }
    }
}
