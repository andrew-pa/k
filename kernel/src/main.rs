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
#![feature(linked_list_cursors)]

extern crate alloc;

mod dtb;

mod exception;
mod memory;
mod process;

mod bus;
mod storage;
mod timer;
mod uart;

use core::{arch::global_asm, panic::PanicInfo};

use alloc::boxed::Box;
use hashbrown::HashMap;
use smallvec::SmallVec;

pub type CHashMapG<K, V> =
    chashmap::CHashMap<K, V, hashbrown::hash_map::DefaultHashBuilder, spin::RwLock<()>>;
pub type CHashMapGReadGuard<'a, K, V> =
    chashmap::ReadGuard<'a, K, V, hashbrown::hash_map::DefaultHashBuilder, spin::RwLock<()>>;
pub type CHashMapGWriteGuard<'a, K, V> =
    chashmap::WriteGuard<'a, K, V, hashbrown::hash_map::DefaultHashBuilder, spin::RwLock<()>>;

global_asm!(include_str!("start.S"));

fn fib(n: usize) -> usize {
    if n < 2 {
        1
    } else {
        fib(n - 1) + fib(n - 2)
    }
}

pub unsafe fn test_thread_code_a() -> ! {
    loop {
        // log::info!("hello from thread A! {}", fib(30));
        // it is impossible to do anything interesting here without mapping the entire kernel
        core::arch::asm!(
            "mov x0, #0x0000fff000000000",
            "mov x1, #65",
            "str x1, [x0]",
            "svc #0xabcd"
        )
    }
}

pub fn test_thread_code_b() -> ! {
    loop {
        log::info!("hello from thread B! {}", fib(30));
    }
}

fn create_test_threads() {
    // create a way unsafe ad-hoc thread
    let stack_a_start = {
        memory::physical_memory_allocator()
            .alloc_contig(1024)
            .expect("allocate stack")
    };
    let mut process_page_table =
        memory::paging::PageTable::empty(false, 0x1a).expect("new page table");
    let code_kva = memory::VirtualAddress(test_thread_code_a as usize);
    let (_, _, _, _, _, po) = code_kva.to_parts();
    log::debug!("code at {code_kva} in kernel, {po:x} in process");
    let pto = memory::paging::PageTableEntryOptions {
        read_only: false,
        el0_access: true,
    };
    process_page_table
        .map_range(
            unsafe { memory::VirtualAddress(code_kva.0 - po).to_physical_canonical() },
            memory::VirtualAddress(0),
            8,
            true,
            &pto,
        )
        .unwrap();
    process_page_table
        .map_range(
            stack_a_start,
            memory::VirtualAddress(0x0000_ffff_0000_0000),
            1024,
            true,
            &pto,
        )
        .unwrap();
    process_page_table
        .map_range(
            memory::PhysicalAddress(0x0900_0000),
            memory::VirtualAddress(0x0000_fff0_0000_0000),
            1,
            true,
            &pto,
        )
        .unwrap();
    log::debug!("test process page table = {:#?}", process_page_table);
    process::processes().insert(
        0x1a,
        process::Process {
            id: 0x1a,
            page_tables: process_page_table,
            threads: SmallVec::from_elem(0xa, 1),
        },
    );
    process::threads().insert(
        0xa,
        process::Thread {
            id: 0xa,
            parent: Some(0x1a),
            register_state: exception::Registers::default(),
            program_status: process::SavedProgramStatus::initial_for_el0(),
            pc: memory::VirtualAddress(po),
            sp: memory::VirtualAddress(0x0000_ffff_0000_0000 + 1024 * memory::PAGE_SIZE),
            priority: process::ThreadPriority::Normal,
        },
    );

    let stack_b_start = {
        memory::physical_memory_allocator()
            .alloc_contig(1024)
            .expect("allocate stack")
    };
    process::threads().insert(
        0xb,
        process::Thread {
            id: 0xb,
            parent: None,
            register_state: exception::Registers::default(),
            program_status: process::SavedProgramStatus::initial_for_el1(),
            pc: memory::VirtualAddress(test_thread_code_b as usize),
            sp: unsafe {
                memory::VirtualAddress(
                    stack_b_start.to_virtual_canonical().0 + 1024 * memory::PAGE_SIZE,
                )
            },
            priority: process::ThreadPriority::Normal,
        },
    );
    process::scheduler::scheduler().add_thread(0xa);
    process::scheduler::scheduler().add_thread(0xb);
}

#[inline]
pub fn halt() -> ! {
    loop {
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)) }
    }
}

pub fn current_el() -> usize {
    let mut current_el: usize;
    unsafe {
        core::arch::asm!(
            "mrs {val}, CurrentEL",
            val = out(reg) current_el
        );
    }
    current_el >> 2
}

pub fn mair() -> usize {
    let mut x: usize;
    unsafe {
        core::arch::asm!(
            "mrs {val}, MAIR_EL1",
            val = out(reg) x
        );
    }
    x >> 2
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

    let current_el = current_el();
    log::info!("current EL = {current_el}");
    log::info!("MAIR = 0x{:x}", mair());

    if current_el != 1 {
        todo!("switch from {current_el} to EL1 at boot");
    }

    let dt = unsafe { dtb::DeviceTree::at_address(0xffff_0000_4000_0000 as *mut u8) };
    dt.log();

    // initialize virtual memory and interrupts
    unsafe {
        exception::install_exception_vector_table();
        memory::init_physical_memory_allocator(&dt);
        memory::paging::init_kernel_page_table();

        /* --- Kernel heap is now available --- */

        memory::init_virtual_address_allocator();
        exception::init_interrupts(&dt);
        process::scheduler::init_scheduler(process::IDLE_THREAD);
    }

    log::info!("kernel systems initialized");

    // initialize PCIe bus and devices
    let mut pcie_drivers = HashMap::new();
    pcie_drivers.insert(
        0x01080200,
        storage::nvme::init_nvme_over_pcie as bus::pcie::DriverInitFn,
    );
    bus::pcie::init(&dt, &pcie_drivers);

    panic!("stop before starting thread scheduler");

    // create idle thread
    process::threads().insert(
        process::IDLE_THREAD,
        process::Thread {
            id: process::IDLE_THREAD,
            parent: None,
            register_state: exception::Registers::default(),
            program_status: process::SavedProgramStatus::initial_for_el1(),
            pc: memory::VirtualAddress(0),
            sp: memory::VirtualAddress(0),
            priority: process::ThreadPriority::Low,
        },
    );

    create_test_threads();

    exception::system_call_handlers().insert(0xabcd, |_, _| {
        log::trace!("test system call {}", fib(31));
    });
    log::debug!("{:?}", exception::system_call_handlers());

    // initialize system timer and interrupt
    let props = timer::find_timer_properties(&dt);
    log::debug!("timer properties = {props:?}");
    let timer_irq = props.interrupt;

    {
        let ic = exception::interrupt_controller();
        ic.set_target_cpu(timer_irq, 0x1);
        ic.set_priority(timer_irq, 0);
        ic.set_config(timer_irq, exception::InterruptConfig::Level);
        ic.set_pending(timer_irq, false);
        ic.set_enable(timer_irq, true);
    }

    timer::set_enabled(true);
    timer::set_interrupts_enabled(true);

    exception::interrupt_handlers().insert(timer_irq, |id, regs| {
        log::trace!("{id} timer interrupt! {}", timer::counter());
        process::scheduler::run_scheduler(regs);
        timer::write_timer_value(timer::frequency() >> 5);
    });

    {
        let kernel_map = memory::paging::kernel_table();
        log::debug!("kernel table {:#?}", kernel_map);
        memory::heap::log_heap_info(log::Level::Debug);
    }

    // set timer to go off after we halt
    timer::write_timer_value(timer::frequency() >> 4);

    // enable all interrupts in DAIF process state mask
    exception::write_interrupt_mask(exception::InterruptMask(0));

    let msi = exception::interrupt_controller()
        .alloc_msi()
        .expect("alloc MSI");
    log::debug!("allocated test MSI {msi:?}");
    unsafe {
        let msi_reg: *mut u32 = msi.register_addr.to_virtual_canonical().as_ptr();
        log::debug!("writing at {:x}", msi_reg as usize);
        msi_reg.write_volatile(msi.data_value as u32);
    }

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
 *  + switching between user/kernel space
 *  + process scheduling
 *  - system calls
 *  - message passing
 *  - shared memory
 *  - file system
 */
