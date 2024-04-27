//! Definitions and drivers for platform devices (CPU, timer, UART).
use core::arch::global_asm;

pub mod intrinsics;
pub mod psci;
pub mod timer;
pub mod uart;

pub type CpuId = usize;

global_asm!(include_str!("start.S"));
