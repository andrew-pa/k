//! System timer driver.

// TODO: thread sync? I think that these are per-core
// TODO: other ELs?
// TODO: physical vs virtual timers?

use bitfield::bitfield;
use spin::once::Once;

use core::arch::asm;

use crate::{ds::dtb::DeviceTree, exception};

/// Read the compare value register (`CNTP_CVAL_EL0`).
pub fn read_compare_value() -> u64 {
    let mut cv: u64;
    unsafe {
        asm!("mrs {cv}, CNTP_CVAL_EL0", cv = out(reg) cv);
    }
    cv
}

/// Write the compare value register (`CNTP_CVAL_EL0`).
pub fn write_compare_value(compare_value: u64) {
    unsafe {
        asm!("msr CNTP_CVAL_EL0, {cv}", cv = in(reg) compare_value);
    }
}

/// Read timer value register (`CNTP_TVAL_EL0`).
pub fn read_timer_value() -> u32 {
    let mut tv: u64;
    unsafe {
        asm!("mrs {tv}, CNTP_TVAL_EL0", tv = out(reg) tv);
    }
    tv as u32
}

/// Write timer value register (`CNTP_TVAL_EL0`).
pub fn write_timer_value(timer_value: u32) {
    unsafe {
        asm!("msr CNTP_TVAL_EL0, {cv:x}", cv = in(reg) timer_value);
    }
}

/// Read timer counter register (`CNTPCT_EL0`).
pub fn counter() -> u64 {
    let mut cntpct: u64;
    unsafe {
        asm!("mrs {val}, CNTPCT_EL0", val = out(reg) cntpct);
    }
    cntpct
}

/// Read timer counter frequency register (`CNTFRQ_EL0`).
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

/// Read the timer control register (`CNTP_CTL_EL0`).
fn read_control() -> TimerControlRegister {
    let mut ctrl: u64;
    unsafe {
        asm!("mrs {ctrl}, CNTP_CTL_EL0", ctrl = out(reg) ctrl);
    }
    TimerControlRegister(ctrl)
}

/// Write the timer control register (`CNTP_CTL_EL0`).
fn write_control(r: TimerControlRegister) {
    unsafe {
        asm!("msr CNTP_CTL_EL0, {ctrl}", ctrl = in(reg) r.0);
    }
}

/// Check to see if the timer condition has been met.
pub fn condition_met() -> bool {
    read_control().istatus()
}

/// Check to see if the timer interrupt is enabled.
pub fn interrupts_enabled() -> bool {
    !read_control().imask()
}

/// Enable/disable the timer interrupt.
pub fn set_interrupts_enabled(enabled: bool) {
    let mut c = read_control();
    c.set_imask(!enabled);
    write_control(c);
}

/// Check if the timer is enabled.
pub fn enabled() -> bool {
    read_control().enable()
}

/// Enable/disable the timer.
pub fn set_enabled(enabled: bool) {
    let mut c = read_control();
    c.set_enable(enabled);
    write_control(c);
}

/// Device tree timer properties.
#[derive(Debug)]
pub struct TimerProperties {
    pub interrupt: crate::exception::InterruptId,
}

impl TimerProperties {
    /// Locate and parse timer properties in a device tree blob.
    /// Panics if the timer node could not be found.
    pub fn from_device_tree(device_tree: &DeviceTree) -> TimerProperties {
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
}

static TIMER_PROPS: Once<TimerProperties> = Once::new();

pub fn init(device_tree: &DeviceTree) {
    TIMER_PROPS.call_once(|| TimerProperties::from_device_tree(device_tree));
}

pub fn properties() -> &'static TimerProperties {
    TIMER_PROPS.get().expect("timer properties discovered")
}
