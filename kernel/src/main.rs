#![no_std]
#![no_main]
#![feature(int_roundings)]
#![feature(lang_items)]
#![feature(is_some_and)]
#![recursion_limit = "256"]

use core::{fmt::Write, panic::PanicInfo, arch::global_asm};

use crate::memory::{paging::{PageTable, TranslationControlReg}, physical_memory_allocator, PhysicalAddress, PAGE_SIZE, VirtualAddress};

mod dtb;
mod memory;
mod uart;

global_asm!(include_str!("start.S"));

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

pub fn test_fn(id_map: &mut PageTable, mem_start: PhysicalAddress, page_count: usize) {
    log::trace!("before unmap");

    id_map.unmap_range(VirtualAddress(mem_start.0), page_count);

    log::trace!("after unmap");

    for i in 0..10 {
        log::trace!("{i}");
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

    let mut current_el = 0usize;
    unsafe {
        core::arch::asm!(
            "mrs {val}, CurrentEL",
            val = out(reg) current_el
        );
    }
    current_el >>= 2;
    log::info!("current EL = {current_el}");

    if current_el != 1 {
        todo!("switch from {current_el} to EL1 at boot");
    }

    let dt = unsafe { dtb::DeviceTree::at_address(0x4000_0000 as *mut u8) };

    // for item in dt.iter_structure() {
    //     log::info!("device tree item: {item:?}");
    // }

    unsafe {
        memory::init_physical_memory_allocator(&dt);
    }

    log::trace!("physical memory allocator initialized!");

    let (mem_start, mem_size) = {
        let pma = physical_memory_allocator();
        (pma.memory_start_addr(), pma.total_memory_size() / PAGE_SIZE)
    };

    let mut identity_map = PageTable::identity(false, mem_start, mem_size).expect("create page table");
    // make sure to keep the UART mapped
    identity_map.map_range(PhysicalAddress(0x09000000), VirtualAddress(0x09000000), 1, true).expect("map uart");
    log::info!("created page table {:#?}", identity_map);

    let mut test_map = PageTable::empty(true).expect("create page table");
    let test_page = {
        physical_memory_allocator().alloc().expect("allocate test page")
    };
    test_map.map_range(test_page, VirtualAddress(0xffff_0000_0000_0000), 1, true).expect("map range");

    test_map.map_range(mem_start, VirtualAddress(0xffff_8000_0000_0000 + mem_start.0), mem_size, true).expect("second id mapping");

    log::info!("created page table {:#?}", test_map);

    // assert_eq!(identity_map.physical_address_of(VirtualAddress(0x46ff4400)), Some(PhysicalAddress(0x46ff4400)));

    let mut mmfr_el1 = 0usize;
    unsafe {
        core::arch::asm!(
            "mrs {val}, ID_AA64MMFR0_EL1",
            val = out(reg) mmfr_el1
        );
    }
    log::debug!("MMFR0_EL1 = {:64b}", mmfr_el1);

    let mut ttbr0_el1 = 0usize;
    unsafe {
        core::arch::asm!(
            "mrs {val}, TTBR0_EL1",
            val = out(reg) ttbr0_el1
        );
    }
    log::debug!("TTBR0_EL1 = {:16x}", ttbr0_el1);

    let mut ttbr1_el1 = 0usize;
    unsafe {
        core::arch::asm!(
            "mrs {val}, TTBR1_EL1",
            val = out(reg) ttbr1_el1
        );
    }
    log::debug!("TTBR1_EL1 = {:16x}", ttbr1_el1);

    unsafe { memory::paging::disable_mmu(); }

    let mut sctlr_el1 = 0usize;
    unsafe {
        core::arch::asm!(
            "mrs {val}, SCTLR_EL1",
            val = out(reg) sctlr_el1
        );
    }
    log::debug!("SCTLR_EL1 = {sctlr_el1:64b}");
    let otcr = unsafe { memory::paging::get_tcr() };
    log::debug!("TCR_EL1  = {:16x} {:?}", otcr.0, otcr);

    let mut tcr = TranslationControlReg(0);
    tcr.set_ha(true);
    tcr.set_hd(true);
    tcr.set_hpd1(true);
    tcr.set_hpd0(true);
    tcr.set_ipas(0b100); //44bits, 16TB. This is what MMFR0_EL1 says in QEMU
    tcr.set_granule_size1(0b10); // 4KiB
    tcr.set_granule_size0(0b00); // 4KiB
    tcr.set_a1(false);
    tcr.set_size_offset1(16);
    tcr.set_size_offset0(16);
    tcr.set_outer_cacheablity1(0b01); // Write-Back, always allocate
    tcr.set_inner_cacheablity1(0b01);
    tcr.set_outer_cacheablity0(0b01);
    tcr.set_inner_cacheablity0(0b01);
    tcr.set_shareability0(0b11);
    log::trace!("TCR_EL1' = {:16x} {:?}", tcr.0, tcr);
    log::trace!("{:16x}", otcr.0 ^ tcr.0);

    // load page tables and activate MMU
    log::info!("activating virtual memory!");
    unsafe {
        identity_map.activate();
        log::trace!("identity map activated");
        test_map.activate();
        log::trace!("test map activated");
        memory::paging::set_tcr(tcr);
        log::trace!("TCR set");
        memory::paging::enable_mmu();
    }
    log::info!("virtual memory activated!");

    let test_ptr_h = (0xffff_0000_0000_0000) as *mut usize;
    // due to the identity mapping, we can still read the physical address
    let test_ptr_l = (test_page.0) as *mut usize;
    let test_ptr_hi = (0xffff_8000_0000_0000 + test_page.0) as *mut usize;
    unsafe {
        let v = 0xabab_bcbc_cdcd_dede;
        test_ptr_h.write(v);
        assert_eq!(v, test_ptr_l.read());
        assert_eq!(v, test_ptr_hi.read());
        let v = !v;
        test_ptr_l.write(v);
        assert_eq!(v, test_ptr_h.read());
        assert_eq!(v, test_ptr_hi.read());
    }

    log::info!("everything works!");

    let test_fn2: fn(&mut PageTable, PhysicalAddress, usize) = unsafe {
        core::mem::transmute::<usize, fn(&mut PageTable, PhysicalAddress, usize)>(test_fn as usize + 0xffff_8000_0000_0000)
    };

    test_fn2(&mut identity_map, mem_start, mem_size);

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
 *      - need to do initial mapping so that we can compile/link the kernel to run at high addresses
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
