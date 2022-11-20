#![no_std]
#![no_main]
#![feature(int_roundings)]
#![feature(lang_items)]
#![recursion_limit = "256"]

use core::{fmt::Write, panic::PanicInfo};

use crate::memory::{paging::PageTable, physical_memory_allocator, PhysicalAddress, PAGE_SIZE};

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

    let mut pt = PageTable::identity(false, mem_start, mem_size).expect("create page table");
    log::info!("created page table {:?}", pt);

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
