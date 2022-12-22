// TODO: thread sync? I think that these are per-core
// TODO: other ELs?
// TODO: physical vs virtual timers?

use core::arch::asm;

use bitfield::bitfield;

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
        asm!("msr CNTP_CVAL_EL0, {cv:w}", cv = in(reg) timer_value);
    }
}

pub fn counter() -> u64 {
    let mut cntpct: u64;
    unsafe {
        asm!("mrs {val}, CNTPCT_EL0", val = out(reg) cntpct);
    }
    cntpct
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
    read_control().imask()
}

pub fn set_interrupts_enabled(enabled: bool) {
    let mut c = read_control();
    c.set_imask(true);
    write_control(c);
}

pub fn timer_enabled() -> bool {
    read_control().enable()
}

pub fn set_timer_enabled(enabled: bool) {
    let mut c = read_control();
    c.set_enable(true);
    write_control(c);
}
