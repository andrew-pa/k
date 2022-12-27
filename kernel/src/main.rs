#![no_std]
#![no_main]
#![recursion_limit = "256"]
#![feature(int_roundings)]
#![feature(lang_items)]
#![feature(is_some_and)]
#![feature(allocator_api)]
#![feature(default_alloc_error_handler)]
#![feature(cstr_from_bytes_until_nul)]
#![feature(once_cell)]

extern crate alloc;

mod dtb;

mod exception;
mod memory;
mod process;

mod timer;
mod uart;

use core::{arch::global_asm, panic::PanicInfo};

use alloc::boxed::Box;

use crate::process::scheduler;

pub type CHashMapG<K, V> =
    chashmap::CHashMap<K, V, hashbrown::hash_map::DefaultHashBuilder, spin::RwLock<()>>;
pub type CHashMapGReadGuard<'a, K, V> =
    chashmap::ReadGuard<'a, K, V, hashbrown::hash_map::DefaultHashBuilder, spin::RwLock<()>>;
pub type CHashMapGWriteGuard<'a, K, V> =
    chashmap::WriteGuard<'a, K, V, hashbrown::hash_map::DefaultHashBuilder, spin::RwLock<()>>;

global_asm!(include_str!("start.S"));

pub fn test_thread_code_a() -> ! {
    loop {
        log::info!("hello from thread A!");
        halt();
    }
}

pub fn test_thread_code_b() -> ! {
    loop {
        log::info!("hello from thread B!");
        halt();
    }
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

    log::set_logger(&uart::DebugUartLogger).expect("set logger");
    log::set_max_level(log::LevelFilter::Trace);
    log::info!("starting kernel!");

    let mut current_el: usize;
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
        exception::install_exception_vector_table();
        memory::init_physical_memory_allocator(&dt);
        memory::paging::init_kernel_page_table();

        /* --- Kernel heap is now available --- */

        exception::init_interrupts(&dt);
        process::scheduler::init_scheduler();
    }

    /*let mut test_map = PageTable::empty(false).expect("create page table");
    let test_page = {
        physical_memory_allocator()
            .alloc()
            .expect("allocate test page")
    };
    test_map
        .map_range(test_page, VirtualAddress(0x0000_0000_000a_0000), 1, true)
        .expect("map range");

    log::info!("created page table {:#?}", test_map);

    {
        let mut kernel_map = memory::paging::kernel_table();

        let test_page_virt = VirtualAddress(0xffff_abcd_0000_0000);
        kernel_map
            .map_range(test_page, test_page_virt, 1, true)
            .expect("map test page");

        log::info!("kernel table {:#?}", kernel_map);

        let v = 0xabcd_ef11_abcd_ef22;
        let x: *mut usize = test_page_virt.as_ptr();
        unsafe {
            x.write(v);
            let v2 = test_page.to_virtual_canonical().as_ptr::<usize>().read();
            log::debug!("{v} {v2}");
            assert_eq!(v, v2);
        }

        unsafe {
            test_map.activate();
            memory::paging::flush_tlb_total();
        }

        unsafe {
            let y = VirtualAddress(0xa_0000).as_ptr::<usize>().read();
            log::debug!("{y}");
            assert_eq!(v, y);
        }
    }*/

    /*let mut v = alloc::vec![1, 2, 3, 4, 5];
    memory::heap::log_heap_info();
    log::debug!("{v:?}");
    for i in 0..16000 {
        v.push(i);
    }
    memory::heap::log_heap_info();
    log::debug!("{}", v.len());*/

    {
        let kernel_map = memory::paging::kernel_table();
        log::info!("kernel table {:#?}", kernel_map);
    }

    let props = timer::find_timer_properties(&dt);
    log::info!("timer properties = {props:?}");

    let timer_irq = props.interrupt;

    let ic = exception::interrupt_controller();
    ic.set_target_cpu(timer_irq, 0x1);
    ic.set_priority(timer_irq, 0);
    ic.set_config(timer_irq, exception::InterruptConfig::Level);
    ic.set_pending(timer_irq, false);
    ic.set_enable(timer_irq, true);

    timer::set_enabled(true);
    timer::set_interrupts_enabled(true);
    log::info!("timer enabled = {}", timer::enabled());
    log::info!("timer interrupts = {}", timer::interrupts_enabled());
    log::info!("timer compare value = {}", timer::read_compare_value());
    log::info!("timer frequency = {}", timer::frequency());
    timer::write_timer_value(timer::frequency() >> 1);

    // create a way unsafe ad-hoc thread
    log::info!("{:x}", test_thread_code_b as usize);
    process::threads().insert(
        0xa,
        process::Thread {
            id: 0xa,
            parent: None,
            register_state: exception::Registers::default(),
            program_status: process::SavedProgramStatus::default_at_el1(),
            pc: memory::VirtualAddress(test_thread_code_a as usize),
        },
    );
    process::threads().insert(
        0xb,
        process::Thread {
            id: 0xb,
            parent: None,
            register_state: exception::Registers::default(),
            program_status: process::SavedProgramStatus::default_at_el1(),
            pc: memory::VirtualAddress(test_thread_code_b as usize),
        },
    );
    process::scheduler::scheduler().add_thread(0xa);
    process::scheduler::scheduler().add_thread(0xb);

    exception::interrupt_handlers().insert(timer_irq, |id, regs| {
        log::info!("{id} timer interrupt! {}", timer::counter());
        process::scheduler::run_scheduler(regs);
        timer::write_timer_value(timer::frequency() >> 1);
    });

    // enable all interrupts in DAIF process state mask
    exception::write_interrupt_mask(exception::InterruptMask(0));

    // for i in 0..50 {
    //     let cntpct = timer::counter();
    //     let p = ic.is_pending(timer_irq);
    //     let e = ic.is_enabled(timer_irq);
    //     let a = ic.is_active(timer_irq);
    //     log::info!("{i} timer counter = {cntpct} P:{p} E:{e} A:{a}");
    //     if timer::condition_met() {
    //         log::info!("condition met");
    //         timer::write_timer_value(15000);
    //     }
    // }

    // log::warn!("attempting to generate a page fault...");
    // let fault_addr = VirtualAddress(0xffff_ffff_ffff_ab00);
    // unsafe {
    //     fault_addr.as_ptr::<usize>().write(3);
    // }

    // let x = process::process(0).unwrap();

    log::info!("waiting for interrupts...");
    halt();
}

#[panic_handler]
fn panic_handler(info: &PanicInfo) -> ! {
    use core::fmt::Write;
    let mut uart = uart::DebugUart {
        base: 0xffff_0000_0900_0000 as *mut u8,
    };
    let _ = uart.write_fmt(format_args!("\npanic! {info}\n"));
    halt();
}

/* TODO:
 *  + initialize MMU & provide API for page allocation and changing page tables. also make sure reserved regions on memory are correctly mapped
 *      - you can identity map the page the instruction ptr is in and then jump elsewhere safely
 *      - need to do initial mapping so that we can compile/link the kernel to run at high addresses
 *  + kernel heap/GlobalAlloc impl
 *  + set up interrupt handlers
 *  + start timer interrupt
 *  - switching between user/kernel space
 *  - process scheduling
 *  - system calls
 *  - message passing
 *  - shared memory
 *  - file system
 */
