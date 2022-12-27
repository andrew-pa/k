use core::{arch::global_asm, cell::OnceCell, fmt::Display};

use bitfield::bitfield;

use crate::{timer, CHashMapG};

pub type InterruptId = u32;

pub mod gic;

pub enum InterruptConfig {
    Edge,
    Level,
}

pub trait InterruptController {
    fn is_enabled(&self, id: InterruptId) -> bool;
    fn set_enable(&self, id: InterruptId, enabled: bool);

    fn is_pending(&self, id: InterruptId) -> bool;
    fn set_pending(&self, id: InterruptId, enabled: bool);

    fn is_active(&self, id: InterruptId) -> bool;
    fn set_active(&self, id: InterruptId, enabled: bool);

    fn priority(&self, id: InterruptId) -> u8;
    fn set_priority(&self, id: InterruptId, priority: u8);

    fn target_cpu(&self, id: InterruptId) -> u8;
    fn set_target_cpu(&self, id: InterruptId, target_cpu: u8);

    fn config(&self, id: InterruptId) -> InterruptConfig;
    fn set_config(&self, id: InterruptId, config: InterruptConfig);

    fn ack_interrupt(&self) -> InterruptId;
    fn finish_interrupt(&self, id: InterruptId);
}

pub type InterruptHandler = fn(InterruptId, *mut Registers);

static mut IC: OnceCell<&'static dyn InterruptController> = OnceCell::new();
static mut INTERRUPT_HANDLERS: OnceCell<CHashMapG<InterruptId, InterruptHandler>> = OnceCell::new();

pub unsafe fn init_interrupts(device_tree: &crate::dtb::DeviceTree) {
    use alloc::boxed::Box;

    // for now this is our only implementation
    let gic = gic::GenericInterruptController::in_device_tree(device_tree).expect("find/init GIC");

    IC.set(Box::leak(Box::new(gic)))
        .ok()
        .expect("init interrupt controller once");
    INTERRUPT_HANDLERS
        .set(Default::default())
        .ok()
        .expect("init interrupts once");
}

pub fn interrupt_controller() -> &'static dyn InterruptController {
    // SAFETY: this basically puts thread safety onto the IC implementation
    unsafe { *IC.get().expect("interrupt controller initialized") }
}

pub fn interrupt_handlers() -> &'static CHashMapG<InterruptId, InterruptHandler> {
    unsafe { INTERRUPT_HANDLERS.get().expect("init interrupts") }
}

bitfield! {
    pub struct InterruptMask(u64);
    u8;
    pub debug, set_debug: 9;
    pub sys_error, set_sys_error: 8;
    pub irq, set_irq: 7;
    pub frq, set_frq: 6;
}

pub fn read_interrupt_mask() -> InterruptMask {
    let mut v: u64;
    unsafe {
        core::arch::asm!("mrs {v}, DAIF", v = out(reg) v);
    }
    InterruptMask(v)
}

pub fn write_interrupt_mask(m: InterruptMask) {
    unsafe {
        core::arch::asm!("msr DAIF, {v}", v = in(reg) m.0);
    }
}

global_asm!(include_str!("table.S"));

#[derive(Debug, Default, Copy, Clone)]
pub struct Registers {
    pub x: [usize; 30],
}

bitfield! {
    pub struct ExceptionSyndromeRegister(u64);
    u8;
    iss2, _: 36, 32;
    ec, _: 31, 26;
    il, _: 25, 25;
    u32, iss, _: 24, 0;
}

impl Display for ExceptionSyndromeRegister {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ESR")
            .field("ISS2", &format_args!("0x{:x}", self.iss2()))
            .field("EC", &format_args!("0b{:b}", self.ec()))
            .field("IL", &self.il())
            .field(
                "ISS",
                &format_args!("0x{:x}=0b{:b}", self.iss(), self.iss()),
            )
            .finish()
    }
}

#[no_mangle]
unsafe extern "C" fn handle_synchronous_exception(regs: *mut Registers, esr: usize, far: usize) {
    panic!(
        "synchronous exception! {}, FAR={far:x}, registers = {:?}",
        ExceptionSyndromeRegister(esr as u64),
        regs.as_ref()
    );
}

#[no_mangle]
unsafe extern "C" fn handle_interrupt(regs: *mut Registers, _esr: usize, _far: usize) {
    let ic = interrupt_controller();
    let id = ic.ack_interrupt();
    log::trace!("interrupt {id}");
    match interrupt_handlers().get(&id) {
        Some(h) => (*h)(id, regs),
        None => log::warn!("unhandled interrupt {id}"),
    }
    ic.finish_interrupt(id);
}

#[no_mangle]
unsafe extern "C" fn handle_fast_interrupt(_regs: *mut Registers, _esr: usize, _far: usize) {
    let ic = interrupt_controller();
    let id = ic.ack_interrupt();
    log::trace!("fast interrupt {id}");
    ic.finish_interrupt(id);
}

#[no_mangle]
unsafe extern "C" fn handle_system_error(regs: *mut Registers, esr: usize, far: usize) {
    panic!(
        "system error! ESR={esr:x}, FAR={far:x}, registers = {:?}",
        regs.as_ref()
    );
}

#[no_mangle]
unsafe extern "C" fn handle_unimplemented_exception(regs: *mut Registers, esr: usize, far: usize) {
    panic!(
        "unimplemented exception! {}, FAR={far:x}, registers = {:?}",
        ExceptionSyndromeRegister(esr as u64),
        regs.as_ref()
    );
}

extern "C" {
    pub fn install_exception_vector_table();
}
