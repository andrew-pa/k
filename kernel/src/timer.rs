//! System timer driver.

// TODO: thread sync? I think that these are per-core
// TODO: other ELs?
// TODO: physical vs virtual timers?

use bitfield::bitfield;

use core::arch::asm;

use crate::{ds::dtb::DeviceTree, exception};

pub fn read_compare_value() -> u64 {
    let mut cv: u64;
    unsafe {
        asm!("mrs {cv}, CNTP_CVAL_EL0", cv = out(reg) cv);
    }
    cv
}

pub fn write_compare_value(compare_value: u64) {
    unsafe {
        asm!("msr CNTP_CVAL_EL0, {cv}", cv = in(reg) compare_value);
    }
}

pub fn read_timer_value() -> u32 {
    let mut tv: u64;
    unsafe {
        asm!("mrs {tv}, CNTP_TVAL_EL0", tv = out(reg) tv);
    }
    tv as u32
}

pub fn write_timer_value(timer_value: u32) {
    unsafe {
        asm!("msr CNTP_TVAL_EL0, {cv:x}", cv = in(reg) timer_value);
    }
}

pub fn counter() -> u64 {
    let mut cntpct: u64;
    unsafe {
        asm!("mrs {val}, CNTPCT_EL0", val = out(reg) cntpct);
    }
    cntpct
}

// WARN: the documentation says that hardware doesn't touch this but that it is
// only for software. However the documentation is also unclear on where to read a
// suitable value to write to this register. It is possible that U-boot sets this
// for us if we are lucky, but I don't know
pub fn frequency() -> u32 {
    let mut freq: u64;
    unsafe {
        asm!("mrs {val}, CNTFRQ_EL0", val = out(reg) freq);
    }
    freq as u32
}

bitfield! {
    struct TimerControlRegister(u64);
    impl Debug;
    u8;
    istatus, _: 2;
    imask, set_imask: 1;
    enable, set_enable: 0;
}

fn read_control() -> TimerControlRegister {
    let mut ctrl: u64;
    unsafe {
        asm!("mrs {ctrl}, CNTP_CTL_EL0", ctrl = out(reg) ctrl);
    }
    TimerControlRegister(ctrl)
}

fn write_control(r: TimerControlRegister) {
    unsafe {
        asm!("msr CNTP_CTL_EL0, {ctrl}", ctrl = in(reg) r.0);
    }
}

pub fn condition_met() -> bool {
    read_control().istatus()
}

pub fn interrupts_enabled() -> bool {
    !read_control().imask()
}

pub fn set_interrupts_enabled(enabled: bool) {
    let mut c = read_control();
    c.set_imask(!enabled);
    write_control(c);
}

pub fn enabled() -> bool {
    read_control().enable()
}

pub fn set_enabled(enabled: bool) {
    let mut c = read_control();
    c.set_enable(enabled);
    write_control(c);
}

#[derive(Debug)]
pub struct TimerProperties {
    pub interrupt: crate::exception::InterruptId,
}

pub fn find_timer_properties(device_tree: &DeviceTree) -> TimerProperties {
    let mut interrupt = None;

    device_tree.process_properties_for_node("timer", |name, data, _| {
        if name == "interrupts" {
            let intc = exception::interrupt_controller();
            let ib = intc.device_tree_interrupt_spec_byte_size();
            // read the second interrupt spec, which should be the virtual timer
            let spec = intc.parse_interrupt_spec_from_device_tree(&data[ib..2 * ib]);
            log::trace!("got timer interrupt spec {spec:?}");
            // TODO: do we care about edge vs level?
            interrupt = spec.map(|(id, _)| id); // Some(30);
        }
    });

    TimerProperties {
        interrupt: interrupt.expect("found timer interrupt"),
    }
}
