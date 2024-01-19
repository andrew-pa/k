use bitfield::{bitfield, Bit, BitMut};

use super::Type0ConfigHeader;

pub struct MsiXCapability {
    block: *mut u8,
}

// byte offset from start of block
const MSGCTRL: isize = 0x02;
const TABLE_OFFSET: isize = 0x04;
const PBA_OFFSET: isize = 0x08;

impl MsiXCapability {
    pub fn enable(&self) {
        unsafe {
            let msg_ctrl = self.block.offset(MSGCTRL + 1); //only the high byte
            msg_ctrl.write_volatile(msg_ctrl.read_volatile() | 0x80);
        }
    }

    pub fn table_size(&self) -> u16 {
        unsafe {
            let msg_ctrl = self.block.offset(MSGCTRL) as *mut u16;
            (msg_ctrl.read_volatile() & 0x03ff) + 1
        }
    }

    /// (which BAR to use, offset from that BAR)
    pub fn table_address(&self) -> (usize, u32) {
        let x = unsafe { (self.block.offset(TABLE_OFFSET) as *mut u32).read_volatile() };
        ((x & 0b111) as usize, x & !0b111)
    }

    /// (which BAR to use, offset from that BAR)
    pub fn pending_bit_array_address(&self) -> (usize, u32) {
        let x = unsafe { (self.block.offset(PBA_OFFSET) as *mut u32).read_volatile() };
        ((x & 0b111) as usize, x & !0b111)
    }

    pub fn at_address(block: *mut u8) -> MsiXCapability {
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

pub struct MsiXTable {
    base: *mut u32,
    size: usize,
}

impl MsiXTable {
    pub fn from_config(header: &Type0ConfigHeader, caps: &MsiXCapability) -> MsiXTable {
        let (bar_ix, offset) = caps.table_address();
        let base_address = header.base_address(bar_ix) + offset as u64;

        MsiXTable {
            base: base_address as *mut u32,
            size: caps.table_size() as usize,
        }
    }

    pub fn len(&self) -> usize {
        self.size
    }

    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    pub fn set_mask(&self, index: usize, masked: bool) {
        assert!(index < self.size);
        unsafe {
            let vctrl = self.base.add(4 * index + 3);
            let mut v = vctrl.read_volatile();
            v.set_bit(0, masked);
            vctrl.write_volatile(v);
        }
    }

    /// Writes an MSI descriptor into the table at index
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

unsafe impl Send for MsiXTable {}
