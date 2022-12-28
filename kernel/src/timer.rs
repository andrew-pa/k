// TODO: thread sync? I think that these are per-core
// TODO: other ELs?
// TODO: physical vs virtual timers?

use bitfield::bitfield;
use byteorder::{BigEndian, ByteOrder};
use core::{arch::asm, ffi::CStr};

use crate::dtb::{DeviceTree, StructureItem};

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
    let mut found_node = false;
    let mut interrupt = None;
    let mut dt = device_tree.iter_structure();

    while let Some(n) = dt.next() {
        match n {
            StructureItem::StartNode(name) if name.starts_with("timer") => {
                found_node = true;
            }
            StructureItem::StartNode(_) if found_node => {
                while let Some(j) = dt.next() {
                    if let StructureItem::EndNode = j {
                        break;
                    }
                }
            }
            StructureItem::EndNode if found_node => break,
            StructureItem::Property { name, data, .. } if found_node => match name {
                "interrupts" => {
                    let mut i = 0;
                    while i < data.len() {
                        let ty = BigEndian::read_u32(&data[i..]);
                        i += 4;
                        let irq = BigEndian::read_u32(&data[i..]);
                        i += 4;
                        let flags = BigEndian::read_u32(&data[i..]);
                        i += 4;
                        log::debug!(
                            "timer interrupt at {irq} with type={ty:x} and flags={flags:x}"
                        );
                    }
                    // TODO: this is the actual IRQ that we get. why 30?! that's not in the device tree!
                    interrupt = Some(30);
                }
                _ => {}
            },
            _ => {}
        }
    }

    TimerProperties {
        interrupt: interrupt.expect("found timer interrupt"),
    }
}
