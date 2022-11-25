#![no_std]
#![no_main]
#![feature(int_roundings)]
#![feature(lang_items)]
#![recursion_limit = "256"]

use core::{fmt::Write, panic::PanicInfo};

use crate::memory::{paging::{PageTable, TranslationControlReg}, physical_memory_allocator, PhysicalAddress, PAGE_SIZE, VirtualAddress};

mod dtb;
mod memory;
mod uart;

struct DebugUart {
    base: *mut u8,
}

impl Write for DebugUart {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for b in s.bytes() {
            unsafe {
                self.base.write_volatile(b);
            }
        }
        Ok(())
    }
}

struct DebugUartLogger;

impl log::Log for DebugUartLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        //WARN: this is currently NOT thread safe!
        let mut uart = DebugUart {
            base: 0x09000000 as *mut u8,
        };
        writeln!(
            uart,
            "[{:<5} ({}:{}) {}] {}",
            record.level(),
            record.file().unwrap_or("unknown file"),
            record.line().unwrap_or(0),
            record.module_path().unwrap_or("unknown module"),
            record.args()
        )
        .unwrap();
    }

    fn flush(&self) {}
}

#[inline]
pub fn halt() -> ! {
    loop {
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)) }
    }
}

#[no_mangle]
pub extern "C" fn kmain() {
    // make sure the BSS section is zeroed
    unsafe {
        memory::zero_bss_section();
    }

    log::set_logger(&DebugUartLogger).expect("set logger");
    log::set_max_level(log::LevelFilter::Trace);
    log::info!("starting kernel!");

    let dt = unsafe { dtb::DeviceTree::at_address(0x4000_0000 as *mut u8) };

    // for item in dt.iter_structure() {
    //     log::info!("device tree item: {item:?}");
    // }

    unsafe {
        memory::init_physical_memory_allocator(&dt);
    }

    {
        let mut phys_mem_al = physical_memory_allocator();
        let addr = phys_mem_al.alloc_contig(3).unwrap();
        log::info!("allocated 3 pages at {}", addr);
        phys_mem_al.free_pages(addr, 3);
        log::info!("freed 3 pages at {}", addr);
    }

    let x = memory::paging::PageTableEntry::table_desc(PhysicalAddress(0xaaaa_bbbb_cccc_dddd));
    log::info!("table desc = 0x{:016x}", x.0);
    for lvl in 1..3 {
        let x = memory::paging::PageTableEntry::block_entry(
            PhysicalAddress(0xaaaa_bbbb_cccc_dddd),
            lvl,
        );
        log::info!("block desc (lvl={lvl}) = 0x{:016x}", x.0);
    }
    let x = memory::paging::PageTableEntry::page_entry(PhysicalAddress(0xaaaa_bbbb_cccc_dddd));
    log::info!("page desc = 0x{:016x}", x.0);

    let (mem_start, mem_size) = {
        let pma = physical_memory_allocator();
        (pma.memory_start_addr(), pma.total_memory_size() / PAGE_SIZE)
    };

    let mut identity_map = PageTable::identity(false, mem_start, mem_size).expect("create page table");
    log::info!("created page table {:?}", identity_map);

    let mut test_map = PageTable::empty(true).expect("create page table");
    let test_page = {
        physical_memory_allocator().alloc().expect("allocate test page")
    };
    test_map.map_range(test_page, VirtualAddress(0xffff_0000_0000_0000), 1, true).expect("map range");
    log::info!("created page table {:?}", test_map);

    // load page tables and activate MMU
    log::info!("activating virtual memory!");
    unsafe {
        identity_map.activate();
        test_map.activate();
        let mut tcr = TranslationControlReg(0);
        tcr.set_ha(true);
        tcr.set_hd(true);
        tcr.set_hpd1(true);
        tcr.set_hpd0(true);
        tcr.set_ipas(0b101); //48bits, 256TB
        tcr.set_granule_size1(0b10); // 4KiB
        tcr.set_granule_size0(0b10); // 4KiB
        tcr.set_a1(false);
        tcr.set_size_offset1(16);
        tcr.set_size_offset0(16);
        tcr.set_outer_cacheablity1(0b01); // Write-Back, always allocate
        tcr.set_inner_cacheablity1(0b01);
        tcr.set_inner_cacheablity0(0b01);
        memory::paging::set_tcr(tcr);
        memory::paging::enable_mmu();
    }
    log::info!("virtual memory activated!");

    let test_ptr_h = (0xffff_0000_0000_0000) as *mut usize;
    // due to the identity mapping, we can still read the physical address
    let test_ptr_l = (test_page.0) as *mut usize;
    unsafe {
        let v = 0xabab_bcbc_cdcd_dede;
        test_ptr_h.write(v);
        assert_eq!(v, test_ptr_l.read());
        let v = !v;
        test_ptr_l.write(v);
        assert_eq!(v, test_ptr_h.read());
    }

    halt();
}

#[panic_handler]
fn panic_handler(info: &PanicInfo) -> ! {
    let mut uart = DebugUart {
        base: 0x09000000 as *mut u8,
    };
    let _ = uart.write_fmt(format_args!("\npanic! {info}"));
    halt();
}

/* TODO:
 *  - initialize MMU & provide API for page allocation and changing page tables. also make sure reserved regions on memory are correctly mapped
 *      - you can identity map the page the instruction ptr is in and then jump elsewhere safely
 *  - kernel heap/GlobalAlloc impl
 *  - set up interrupt handlers
 *  - start timer interrupt
 *  - switching between user/kernel space
 *  - process scheduling
 *  - system calls
 *  - message passing
 *  - shared memory
 *  - file system
 */
