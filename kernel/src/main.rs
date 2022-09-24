#![feature(lang_items)]
#![no_std]
#![no_main]

use core::{panic::PanicInfo, fmt::Write};

use byteorder::{ByteOrder, BigEndian};

mod dtb;
mod uart;
mod memory;

struct DebugUart {
    base: *mut u8
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
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        //WARN: this is currently NOT thread safe!
        let mut uart = DebugUart { base: 0x09000000 as *mut u8 };
        writeln!(uart, "[{:<5} ({}:{}) {}] {}",
            record.level(),
            record.file().unwrap_or("unknown file"),
            record.line().unwrap_or(0),
            record.module_path().unwrap_or("unknown module"),
            record.args()
        ).unwrap();
    }

    fn flush(&self) { }
}

#[inline]
pub fn halt() -> ! {
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack))
        }
    }
}

#[no_mangle]
pub extern fn kmain() {
    // make sure the BSS section is zeroed
    unsafe { memory::zero_bss_section(); }

    log::set_logger(&DebugUartLogger).expect("set logger");
    log::set_max_level(log::LevelFilter::Trace);
    log::info!("starting kernel!");

    let dt = unsafe { dtb::DeviceTree::at_address(0x4000_0000 as *mut u8) };

    for (addr, size) in dt.iter_reserved_memory_regions() {
        log::info!("reserved memory region at 0x{addr:x}, size={size}");
    }

    // for item in dt.iter_structure() {
    //     log::info!("device tree item: {item:?}");
    // }
    
    // for now, find the first memory node and use it to determine how big RAM is
    let memory_props = dt.iter_structure().skip_while(|i| match i {
        dtb::StructureItem::StartNode(name) if name.starts_with("memory") => false,
        _ => true
    }).find_map(|i| match i {
        dtb::StructureItem::Property { name, data } if name == "reg" => Some(data),
        _ => None
    }).expect("RAM properties in device tree");
    let mem_start = BigEndian::read_u64(&memory_props);
    let mem_length = BigEndian::read_u64(&memory_props[8..]);
    log::info!("RAM starts at 0x{mem_start:x} and is 0x{mem_length:x} bytes long");

    halt();
}

#[panic_handler]
fn panic_handler(info: &PanicInfo) -> ! {
    let mut uart = DebugUart { base: 0x09000000 as *mut u8 };
    let _ = uart.write_fmt(format_args!("panic! {info}"));
    halt();
}

/* TODO:
 *  - initialize MMU & provide API for page allocation and changing page tables. also make sure reserved regions on memory are correctly mapped
 *  - set up interrupt handlers
 *  - start timer interrupt
 *  - switching between user/kernel space
 *  - process scheduling
 *  - system calls
 *  - message passing
 *  - shared memory
 *  - file system
 */
