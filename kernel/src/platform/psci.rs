//! Client interface for the ARM Power State Coordination Interface (PSCI).
//!
//! API Reference: <https://developer.arm.com/documentation/den0022>
//!
//! Device Tree Reference: <https://www.kernel.org/doc/Documentation/devicetree/bindings/arm/psci.txt>

use bytemuck::Contiguous;
use byteorder::{BigEndian, ByteOrder};
use snafu::Snafu;

use crate::{
    ds::dtb::DeviceTree,
    memory::{PhysicalAddress, VirtualAddress},
};

use super::CpuId;

/// Error codes as defined by §5.2.2.
#[derive(Debug, Snafu, Contiguous, Copy, Clone)]
#[snafu(module(error_selectors))]
#[repr(i32)]
pub enum Error {
    Unknown = 0,
    NotSupported = -1,
    InvalidParameters = -2,
    Denied = -3,
    AlreadyOn = -4,
    OnPending = -5,
    InternalFailure = -6,
    NotPresent = -7,
    Disabled = -8,
    InvalidAddress = -9,
    Timeout = -10,
    RateLimited = -11,
    Busy = -12,
}

/// Method for invoking a PSCI firmware function.
#[derive(Debug)]
enum CallingMethod {
    /// Use the SMC instruction.
    Smc,
    /// Use the HVC instruction.
    Hvc,
}

/// Function ID for `CPU_ON` PSCI function.
const FUNC_ID_CPU_ON: u32 = 0xC400_0003;

/// The PSCI interface.
#[derive(Debug)]
pub struct Psci {
    /// The calling method reported by the firmware.
    calling_method: CallingMethod,
    /// The current function ID for `CPU_ON` PSCI function reported by the firmware.
    func_id_cpu_on: u32,
}

impl Psci {
    /// Create a new client using device tree information for portability.
    pub fn new(dt: &DeviceTree) -> Option<Self> {
        let mut calling_method = None;
        let mut func_id_cpu_on = None;

        let found = dt.process_properties_for_node("psci", |name, data, _| match name {
            "method" => {
                calling_method = match data {
                    b"smc\0" => Some(CallingMethod::Smc),
                    b"hvc\0" => Some(CallingMethod::Hvc),
                    _ => None,
                };
            }
            "cpu_on" => func_id_cpu_on = Some(BigEndian::read_u32(data)),
            _ => {}
        });

        if found {
            if let Some(calling_method) = calling_method {
                Some(Self {
                    calling_method,
                    func_id_cpu_on: func_id_cpu_on.unwrap_or(FUNC_ID_CPU_ON),
                })
            } else {
                log::warn!(
                    "found PSCI block in device tree without valid calling method, ignoring"
                );
                None
            }
        } else {
            None
        }
    }

    /// Power on a core that is current off, instructing it to start at `entry_point`.
    ///
    /// The `context_id` value will be passed to the entry point as the first argument.
    pub fn cpu_on(
        &self,
        target_cpu: CpuId,
        entry_point_address: PhysicalAddress,
        context_id: usize,
    ) -> Result<(), Error> {
        log::debug!("turning CPU #{target_cpu} on! entry point = {entry_point_address}, context id = 0x{context_id:x}");

        let result: i32;

        unsafe {
            match self.calling_method {
                CallingMethod::Smc => core::arch::asm!(
                    "mov w0, {func_id:w}",
                    "mov x1, {target_cpu}",
                    "mov x2, {entry_point_address}",
                    "mov x3, {context_id}",
                    "smc #0",
                    "mov {result:w}, w0",
                    func_id = in(reg) self.func_id_cpu_on,
                    entry_point_address = in(reg) entry_point_address.0,
                    target_cpu = in(reg) target_cpu,
                    context_id = in(reg) context_id,
                    result = out(reg) result
                ),
                CallingMethod::Hvc => core::arch::asm!(
                    "mov w0, {func_id:w}",
                    "mov x1, {target_cpu}",
                    "mov x2, {entry_point_address}",
                    "mov x3, {context_id}",
                    "hvc #0",
                    "mov {result:w}, w0",
                    func_id = in(reg) self.func_id_cpu_on,
                    entry_point_address = in(reg) entry_point_address.0,
                    target_cpu = in(reg) target_cpu,
                    context_id = in(reg) context_id,
                    result = out(reg) result
                ),
            }
        }

        if result == 0 {
            Ok(())
        } else {
            Err(Error::from_integer(result).unwrap_or(Error::Unknown))
        }
    }
}
