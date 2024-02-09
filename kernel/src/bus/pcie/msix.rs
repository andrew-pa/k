//! The MSI-X (Message Signaled Interrupts eXtended) capability block interface.
//!
//! This module provides a nice interface for configuring a device to use message signaled
//! interrupts, independent of the interrupt controller.
use bitfield::{bitfield, Bit, BitMut};

use super::Type0ConfigHeader;

/// The MSI-X capability block.
pub struct MsiXCapability {
    block: *mut u8,
}

// byte offset from start of block
const MSGCTRL: isize = 0x02;
const TABLE_OFFSET: isize = 0x04;
const PBA_OFFSET: isize = 0x08;

impl MsiXCapability {
    /// Enable using MSI-X with the device.
    pub fn enable(&self) {
        unsafe {
            let msg_ctrl = self.block.offset(MSGCTRL + 1); //only the high byte
            msg_ctrl.write_volatile(msg_ctrl.read_volatile() | 0x80);
        }
    }

    /// Reads the number of MSI-X table entries that are available.
    pub fn table_size(&self) -> u16 {
        unsafe {
            let msg_ctrl = self.block.offset(MSGCTRL) as *mut u16;
            (msg_ctrl.read_volatile() & 0x03ff) + 1
        }
    }

    /// Read the address of the MSI-X interrupt table.
    /// Returns (the base address index in the configuration header to use, offset from that BAR)
    fn table_address(&self) -> (usize, u32) {
        let x = unsafe { (self.block.offset(TABLE_OFFSET) as *mut u32).read_volatile() };
        ((x & 0b111) as usize, x & !0b111)
    }

    /// Read the address of the MSI-X pending bitmap.
    /// Returns (the base address index in the configuration header to use, offset from that BAR)
    fn pending_bit_array_address(&self) -> (usize, u32) {
        let x = unsafe { (self.block.offset(PBA_OFFSET) as *mut u32).read_volatile() };
        ((x & 0b111) as usize, x & !0b111)
    }

    pub(super) fn at_address(block: *mut u8) -> MsiXCapability {
        MsiXCapability { block }
    }
}

impl core::fmt::Debug for MsiXCapability {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MsiXCapability")
            .field("block", &self.block)
            .field("table size", &self.table_size())
            .field("table address", &self.table_address())
            .field("PBA address", &self.pending_bit_array_address())
            .finish()
    }
}

/// The MSI-X interrupt table, which stores the value to write and address to write to when an
/// interrupt occurs.
pub struct MsiXTable {
    base: *mut u32,
    size: usize,
}

impl MsiXTable {
    /// Locate the table from the configuration header and MSI-X capability block.
    pub fn from_config(header: &Type0ConfigHeader, caps: &MsiXCapability) -> MsiXTable {
        let (bar_ix, offset) = caps.table_address();
        let base_address = header.base_address(bar_ix) + offset as u64;

        MsiXTable {
            base: base_address as *mut u32,
            size: caps.table_size() as usize,
        }
    }

    /// The number of entries in the table.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Set the mask for a particular interrupt by index.
    ///
    /// Panics if the index is out-of-bounds.
    pub fn set_mask(&self, index: usize, masked: bool) {
        assert!(index < self.size);
        unsafe {
            let vctrl = self.base.add(4 * index + 3);
            let mut v = vctrl.read_volatile();
            v.set_bit(0, masked);
            vctrl.write_volatile(v);
        }
    }

    /// Writes an MSI descriptor from the interrupt controller into the table at some index.
    ///
    /// The interrupt will start masked, and must be unmasked when the driver is ready to process interrupts
    pub fn write(&self, index: usize, msi: &crate::exception::MsiDescriptor) {
        assert!(index < self.size);
        self.set_mask(index, true);
        unsafe {
            let entry = self.base.add(4 * index);
            (entry as *mut u64).write_volatile(msi.register_addr.0 as u64);
            entry.offset(2).write_volatile(msi.data_value);
        }
    }
}

// TODO: this is most likely wrong
unsafe impl Send for MsiXTable {}
