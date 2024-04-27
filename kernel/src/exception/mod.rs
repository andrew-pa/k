//! Hardware exception (interrupt) handling and routing.
//!
//! This module contains the exception handlers themselves, as well as the interrupt controller
//! device drivers. There is a global interrupt controller implementation that is made available to
//! the rest of the system.
//!
//! Each interrupt controller device driver provides an implementation of [InterruptController]
//! that allows the system to interact with the device abstractly, but only one instance exists at a
//! time, accessable by calling [interrupt_controller].
use core::{arch::global_asm, cell::OnceCell, fmt::Display};

use alloc::{boxed::Box, sync::Arc};
use bitfield::bitfield;
use spin::{Mutex, MutexGuard};

use crate::{
    ds::maps::CHashMap,
    memory::{PhysicalAddress, VirtualAddress},
    process::{self, thread::reg::read_exception_link_reg, Process, Thread},
};

/// An interrupt identifier.
pub type InterruptId = u32;

pub mod gic;

mod handlers;
pub use handlers::install_exception_vector_table;

/// Configuration for an interrupt.
#[derive(Debug)]
pub enum InterruptConfig {
    /// Use edge triggering.
    Edge,
    /// Use level triggering.
    Level,
}

/// Description of a message signaled interrupt setup.
#[derive(Debug)]
pub struct MsiDescriptor {
    /// The address the device should write to.
    pub register_addr: PhysicalAddress,
    /// The value that should be written at the address.
    pub data_value: u32,
    /// The ID of the interrupt that will be generated.
    pub intid: InterruptId,
}

/// An abstract interrupt controller interface.
pub trait InterruptController {
    /// The size in bytes of an interrupt specification in the device tree for this controller.
    fn device_tree_interrupt_spec_byte_size(&self) -> usize;

    /// Tries to read an interrupt specification from a device tree property data slice.
    /// The slice must be of size `device_tree_interrupt_spec_byte_size()`.
    fn parse_interrupt_spec_from_device_tree(
        &self,
        data: &[u8],
    ) -> Option<(InterruptId, InterruptConfig)>;

    /// Check if an interrupt is enabled.
    fn is_enabled(&self, id: InterruptId) -> bool;
    /// Enable or disable an interrupt.
    fn set_enable(&self, id: InterruptId, enabled: bool);

    /// Check if an interrupt is pending.
    fn is_pending(&self, id: InterruptId) -> bool;
    /// Clear a pending interrupt.
    fn clear_pending(&self, id: InterruptId);

    /// Check if an interrupt is active.
    fn is_active(&self, id: InterruptId) -> bool;
    /// Set the active state of an interrupt.
    fn set_active(&self, id: InterruptId, enabled: bool);

    /// Get the priority of an interrupt.
    fn priority(&self, id: InterruptId) -> u8;
    /// Set the priority of an interrupt.
    fn set_priority(&self, id: InterruptId, priority: u8);

    /// Get the target CPU ID for an interrupt.
    fn target_cpu(&self, id: InterruptId) -> u8;
    /// Set the target CPU ID for an interrupt.
    fn set_target_cpu(&self, id: InterruptId, target_cpu: u8);

    /// Get the current configuration of an interrupt.
    fn config(&self, id: InterruptId) -> InterruptConfig;
    /// Set the configuration of an interrupt.
    fn set_config(&self, id: InterruptId, config: InterruptConfig);

    /// Acknowledge that an interrupt exception has been handled. Returns the ID of the interrupt
    /// that was triggered.
    ///
    /// This function is called by the interrupt exception handler before calling the registered
    /// kernel interrupt handler function.
    fn ack_interrupt(&self) -> Option<InterruptId>;
    /// Inform the interrupt controller that the system has finished processing an interrupt.
    ///
    /// This function is called after the registered interrupt function has finished, before the
    /// exception handler returns.
    fn finish_interrupt(&self, id: InterruptId);

    /// Detect if MSIs (message signaled interrupts) are supported by this interrupt controller.
    fn msi_supported(&self) -> bool {
        false
    }

    /// Allocate an MSI on the controller that can be registered with a device.
    ///
    /// Panics if MSIs are not supported.
    // TODO: what happens if we run out of MSIs?
    fn alloc_msi(&mut self) -> Option<MsiDescriptor> {
        unimplemented!("MSIs not supported by interrupt controller");
    }
    // TODO: free MSIs
}

/// A callback that can be registered to handle an interrupt.
///
/// The function is given the ID of the interrupt it is handling and a mutable reference to the
/// current [Registers] of the running thread.
pub type InterruptHandler = Box<dyn FnMut(InterruptId, &mut Registers)>;
/// A callback that can be registered to handle a system call.
///
/// The function is given the number of the system call it is handling,
/// the process and thread that invoked the system call,
/// and a mutable reference to the current [Registers] of the running thread.
pub type SyscallHandler = fn(u16, Arc<Process>, Arc<Thread>, &mut Registers);

static mut IC: OnceCell<Mutex<Box<dyn InterruptController>>> = OnceCell::new();
static mut INTERRUPT_HANDLERS: OnceCell<CHashMap<InterruptId, InterruptHandler>> = OnceCell::new();
static mut SYSTEM_CALL_HANDLERS: OnceCell<CHashMap<u16, SyscallHandler>> = OnceCell::new();

/// Initialize the interrupt controller using information in the device tree.
/// The type of the interrupt controller is automatically detected.
pub fn init_interrupts(device_tree: &crate::ds::dtb::DeviceTree) {
    // for now this is our only implementation
    let gic = gic::GenericInterruptController::in_device_tree(device_tree).expect("find/init GIC");

    unsafe {
        IC.set(Mutex::new(Box::new(gic)))
            .ok()
            .expect("init interrupt controller once");
        INTERRUPT_HANDLERS
            .set(Default::default())
            .ok()
            .expect("init interrupts once");
        SYSTEM_CALL_HANDLERS
            .set(Default::default())
            .ok()
            .expect("init syscall table once");
    }
}

/// Lock and gain access to the current system interrupt controller.
pub fn interrupt_controller() -> MutexGuard<'static, Box<dyn InterruptController>> {
    unsafe { IC.get().expect("interrupt controller initialized").lock() }
}

/// Get the interrupt handler table.
pub fn interrupt_handlers() -> &'static CHashMap<InterruptId, InterruptHandler> {
    unsafe { INTERRUPT_HANDLERS.get().expect("init interrupts") }
}

/// Get the system call handler table.
pub fn system_call_handlers() -> &'static CHashMap<u16, SyscallHandler> {
    unsafe { SYSTEM_CALL_HANDLERS.get().expect("init syscall table") }
}

// In this register, 0 is enabled and 1 is disabled.
bitfield! {
    /// A system CPU exception mask (DAIF register) value.
    pub struct InterruptMask(u64);
    u8;
    debug, set_debug: 9;
    sys_error, set_sys_error: 8;
    irq, set_irq: 7;
    frq, set_frq: 6;
}

impl InterruptMask {
    /// A mask to enable all exceptions.
    pub fn all_enabled() -> InterruptMask {
        InterruptMask(0)
    }

    /// A mask to disable all exceptions.
    pub fn all_disabled() -> InterruptMask {
        let mut s = InterruptMask(0);
        s.set_debug(true);
        s.set_frq(true);
        s.set_irq(true);
        s.set_sys_error(true);
        s
    }
}

/// Read the CPU exception mask register (DAIF).
#[inline]
pub fn read_interrupt_mask() -> InterruptMask {
    let mut v: u64;
    unsafe {
        core::arch::asm!("mrs {v}, DAIF", v = out(reg) v);
    }
    InterruptMask(v)
}

/// Write the CPU exception mask register (DAIF).
///
/// # Safety
/// The system must be ready to accept interrupts as soon as the next instruction if they are
/// enabled.
#[inline]
pub unsafe fn write_interrupt_mask(m: InterruptMask) {
    core::arch::asm!("msr DAIF, {v}", v = in(reg) m.0);
}

/// A guard that disables interrupts until it is dropped, then reenables them.
pub struct InterruptGuard {
    previous_mask: InterruptMask,
}

impl InterruptGuard {
    /// Disable interrupts until the guard is dropped, when the previous interrupt mask will be
    /// restored.
    pub fn disable_interrupts_until_drop() -> Self {
        let previous_mask = read_interrupt_mask();
        unsafe {
            write_interrupt_mask(InterruptMask::all_disabled());
        }
        InterruptGuard { previous_mask }
    }
}

impl Drop for InterruptGuard {
    fn drop(&mut self) {
        unsafe {
            write_interrupt_mask(InterruptMask(self.previous_mask.0));
        }
    }
}

/// A stored version of the system registers x0..x31.
// TODO: this doesn't really belong in this module.
#[derive(Default, Copy, Clone)]
pub struct Registers {
    /// The values of the xN registers in order.
    pub x: [usize; 31],
}

impl Registers {
    /// Create a Registers struct that has the contents of args in the first registers. There can
    /// be up to 8, mirroring the typical ARM64 calling convention.
    pub fn from_args(args: &[usize]) -> Registers {
        assert!(args.len() <= 8);
        let mut regs = Registers::default();
        regs.x[0..args.len()].copy_from_slice(args);
        regs
    }
}

impl core::fmt::Debug for Registers {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut s = f.debug_struct("Registers");
        s.field("x0", &format_args!("0x{:x}", self.x[0]));
        s.field("x1", &format_args!("0x{:x}", self.x[1]));
        s.field("x2", &format_args!("0x{:x}", self.x[2]));
        s.field("x3", &format_args!("0x{:x}", self.x[3]));
        s.field("x4", &format_args!("0x{:x}", self.x[4]));
        s.field("x5", &format_args!("0x{:x}", self.x[5]));
        s.field("x6", &format_args!("0x{:x}", self.x[6]));
        s.field("x7", &format_args!("0x{:x}", self.x[7]));
        s.field("x8", &format_args!("0x{:x}", self.x[8]));
        s.field("x9", &format_args!("0x{:x}", self.x[9]));
        s.field("x10", &format_args!("0x{:x}", self.x[10]));
        s.field("x11", &format_args!("0x{:x}", self.x[11]));
        s.field("x12", &format_args!("0x{:x}", self.x[12]));
        s.field("x13", &format_args!("0x{:x}", self.x[13]));
        s.field("x14", &format_args!("0x{:x}", self.x[14]));
        s.field("x15", &format_args!("0x{:x}", self.x[15]));
        s.field("x16", &format_args!("0x{:x}", self.x[16]));
        s.field("x17", &format_args!("0x{:x}", self.x[17]));
        s.field("x18", &format_args!("0x{:x}", self.x[18]));
        s.field("x19", &format_args!("0x{:x}", self.x[19]));
        s.field("x20", &format_args!("0x{:x}", self.x[20]));
        s.field("x21", &format_args!("0x{:x}", self.x[21]));
        s.field("x22", &format_args!("0x{:x}", self.x[22]));
        s.field("x23", &format_args!("0x{:x}", self.x[23]));
        s.field("x24", &format_args!("0x{:x}", self.x[24]));
        s.field("x25", &format_args!("0x{:x}", self.x[25]));
        s.field("x26", &format_args!("0x{:x}", self.x[26]));
        s.field("x27", &format_args!("0x{:x}", self.x[27]));
        s.field("x28", &format_args!("0x{:x}", self.x[28]));
        s.field("x29", &format_args!("0x{:x}", self.x[29]));
        s.field("x30", &format_args!("0x{:x}", self.x[30]));
        s.finish()
    }
}

bitfield! {
    /// A value in the ESR (Exception Syndrome Register), which indicates the cause of an
    /// exception.
    struct ExceptionSyndromeRegister(u64);
    u8;
    iss2, _: 36, 32;
    ec, _: 31, 26;
    il, _: 25, 25;
    u32, iss, _: 24, 0;
}

struct ExceptionClass(u8);

impl ExceptionClass {
    #[inline]
    fn is_system_call(&self) -> bool {
        self.0 == 0b010101
    }

    #[inline]
    fn is_user_space_data_page_fault(&self) -> bool {
        self.0 == 0b100100
    }

    #[inline]
    fn is_user_space_code_page_fault(&self) -> bool {
        self.0 == 0b100000
    }
}

impl core::fmt::Debug for ExceptionClass {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "0b{:b}=", self.0)?;
        match self.0 {
            0b000000 => write!(f, "[Misc]"),
            0b000001 => write!(f, "[Trapped WF* instruction]"),
            0b000111 => write!(f, "[Access to SME, SVE, Advanced SIMD or floating-point functionality trapped by CPACR_EL1.FPEN, CPTR_EL2.FPEN, CPTR_EL2.TFP, or CPTR_EL3.TFP control]"),
            0b001010 => write!(f, "[Trapped execution of an LD64B or ST64B* instruction.]"),
            0b001101 => write!(f, "[Branch Target Exception]"),
            0b001110 => write!(f, "[Illegal Execution state]"),
            0b010101 => write!(f, "[SVC instruction]"),
            0b011000 => write!(f, "[Trapped MSR, MRS or System instruction execution in AArch64 state, that is not reported using EC 0b000000, 0b000001, or 0b000111]"),
            0b100000 => write!(f, "[Instruction Abort from a lower Exception level]"),
            0b100001 => write!(f, "[Instruction Abort taken without a change in Exception level]"),
            0b100010 => write!(f, "[PC alignment fault exception]"),
            0b100100 => write!(f, "[Data Abort exception from a lower Exception level]"),
            0b100101 => write!(f, "[Data Abort exception taken without a change in Exception level]"),
            0b100110 => write!(f, "[SP alignment fault exception]"),
            _ => write!(f, "[Unknown]")
        }
    }
}

fn dfsc_description(code: u8) -> &'static str {
    match code {
        0b000000 => "Address size fault, level 0 of translation or translation table base register",
        0b000001 => "Address size fault, level 1",
        0b000010 => "Address size fault, level 2",
        0b000011 => "Address size fault, level 3",
        0b000100 => "Translation fault, level 0",
        0b000101 => "Translation fault, level 1",
        0b000110 => "Translation fault, level 2",
        0b000111 => "Translation fault, level 3",
        0b001000 => "Access flag fault, level 0",
        0b001001 => "Access flag fault, level 1",
        0b001010 => "Access flag fault, level 2",
        0b001011 => "Access flag fault, level 3",
        0b001100 => "Permission fault, level 0",
        0b001101 => "Permission fault, level 1",
        0b001110 => "Permission fault, level 2",
        0b001111 => "Permission fault, level 3",
        0b010000 => "Synchronous External abort, not on translation table walk or hardware update of translation table",
        0b010001 => "Synchronous Tag Check Fault",
        0b010011 => "Synchronous External abort on translation table walk or hardware update of translation table, level -1",
        0b010100 => "Synchronous External abort on translation table walk or hardware update of translation table, level 0",
        0b010101 => "Synchronous External abort on translation table walk or hardware update of translation table, level 1",
        0b010110 => "Synchronous External abort on translation table walk or hardware update of translation table, level 2",
        0b010111 => "Synchronous External abort on translation table walk or hardware update of translation table, level 3",
        0b011000 => "Synchronous parity or ECC error on memory access, not on translation table walk",
        0b011011 => "Synchronous parity or ECC error on memory access on translation table walk or hardware update of translation table, level -1",
        0b011100 => "Synchronous parity or ECC error on memory access on translation table walk or hardware update of translation table, level 0",
        0b011101 => "Synchronous parity or ECC error on memory access on translation table walk or hardware update of translation table, level 1",
        0b011110 => "Synchronous parity or ECC error on memory access on translation table walk or hardware update of translation table, level 2",
        0b011111 => "Synchronous parity or ECC error on memory access on translation table walk or hardware update of translation table, level 3",
        0b100001 => "Alignment fault",
        0b100011 => "Granule Protection Fault on translation table walk or hardware update of translation table, level -1",
        0b100100 => "Granule Protection Fault on translation table walk or hardware update of translation table, level 0",
        0b100101 => "Granule Protection Fault on translation table walk or hardware update of translation table, level 1",
        0b100110 => "Granule Protection Fault on translation table walk or hardware update of translation table, level 2",
        0b100111 => "Granule Protection Fault on translation table walk or hardware update of translation table, level 3",
        0b101000 => "Granule Protection Fault, not on translation table walk or hardware update of translation table",
        0b101001 => "Address size fault, level -1",
        0b101011 => "Translation fault, level -1",
        0b110000 => "TLB conflict abort",
        0b110001 => "Unsupported atomic hardware update fault",
        0b110100 => "IMPLEMENTATION DEFINED fault (Lockdown)",
        0b110101 => "IMPLEMENTATION DEFINED fault (Unsupported Exclusive or Atomic access)",
        _ => "?",
    }
}

impl Display for ExceptionSyndromeRegister {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ESR")
            .field("ISS2", &format_args!("0x{:x}", self.iss2()))
            .field("EC", &ExceptionClass(self.ec()))
            .field("IL", &self.il())
            .field(
                "ISS",
                &format_args!(
                    "0x{:x}=0b{:b} \"{}\"",
                    self.iss(),
                    self.iss(),
                    if self.ec() == 0b10_0101 {
                        dfsc_description((self.iss() & 0b11_1111) as u8)
                    } else {
                        ""
                    }
                ),
            )
            .finish()
    }
}
