use bitfield::{bitfield, Bit, BitMut};

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
            msg_ctrl.write_volatile(msg_ctrl.read_volatile() & 0x80);
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
    pub fn from_config(bars: &[u32], caps: &MsiXCapability) -> MsiXTable {
        let (bar_ix, offset) = caps.table_address();
        let barl = bars[bar_ix];
        let base_address = if barl.bit(2) {
            let barh = bars[bar_ix + 1];
            (barl & 0xffff_fff0) as u64 | ((barh as u64) << 32)
        } else {
            (barl & 0xffff_fff0) as u64
        } + offset as u64;

        MsiXTable {
            base: base_address as *mut u32,
            size: caps.table_size() as usize,
        }
    }

    pub fn len(&self) -> usize {
        self.size
    }

    pub fn set_mask(&self, index: usize, masked: bool) {
        assert!(index < self.size);
        unsafe {
            let vctrl = self.base.offset((4 * index) as isize + 3);
            let mut v = vctrl.read_volatile();
            v.set_bit(0, masked);
            vctrl.write_volatile(v);
        }
    }

    pub fn write(&self, index: usize, msi: crate::exception::MsiDescriptor) {
        assert!(index < self.size);
        self.set_mask(index, true);
        unsafe {
            let entry = self.base.offset((4 * index) as isize);
            (entry as *mut u64).write_volatile(msi.register_addr.0 as u64);
            entry.offset(2).write_volatile(msi.data_value);
        }
    }
}
