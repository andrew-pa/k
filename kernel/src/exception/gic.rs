use core::{ffi::CStr, mem::size_of};

use bitvec::{
    index::BitIdx,
    order::Lsb0,
    ptr::{BitPtr, Const, Mut},
};

use crate::{
    dtb::{DeviceTree, MemRegionIter, StructureItem},
    memory::PhysicalAddress,
};

use super::{InterruptConfig, InterruptController, InterruptId};

pub struct GenericInterruptController {
    distributor_base: *mut u32,
    cpu_base: *mut u32,
}

impl GenericInterruptController {
    pub fn in_device_tree(device_tree: &DeviceTree) -> Option<GenericInterruptController> {
        let mut found_node = false;
        let mut distributor_base = None;
        let mut cpu_base = None;
        let mut dt = device_tree.iter_structure();

        while let Some(i) = dt.next() {
            match i {
                StructureItem::StartNode(name) if name.starts_with("intc") => {
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
                    "reg" => {
                        let mut r = MemRegionIter::for_data(data);
                        distributor_base = r.next();
                        cpu_base = r.next();
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        if !found_node || distributor_base.is_none() || cpu_base.is_none() {
            return None;
        }

        let distributor_base = PhysicalAddress(distributor_base.unwrap().0 as usize);
        let cpu_base = PhysicalAddress(cpu_base.unwrap().0 as usize);

        log::info!(
            "GIC: distributor @ {}, cpu interface @ {}",
            distributor_base,
            cpu_base
        );

        // TODO: SAFETY: assume that these are in low memory that has already been mapped
        let mut s = unsafe {
            Self {
                distributor_base: distributor_base.to_virtual_canonical().as_ptr(),
                cpu_base: cpu_base.to_virtual_canonical().as_ptr(),
            }
        };
        s.init_distributor();
        s.init_cpu_interface();
        Some(s)
    }

    fn init_distributor(&mut self) {
        unsafe {
            // enable distributor
            self.distributor_base.offset(GICD_CTLR).write_volatile(0x1);
        }
    }

    fn init_cpu_interface(&mut self) {
        unsafe {
            // enable cpu interface
            // make writes to GICC_EOIR deactive interrupt (bits 9 and 10)
            // TODO: these bits don't seem to eliminate the need to write to GICC_DIR anyways?
            self.cpu_base
                .offset(GICC_CTLR)
                .write_volatile(0b0000_0110_0000_0001);
            // accept interrupts of any priority by setting the minimum priority
            // register to the lowest possible priority
            self.cpu_base.offset(GICC_PMR).write_volatile(0xff);
            // disable group priority bits
            self.cpu_base.offset(GICC_BPR).write_volatile(0x0);
        }
    }

    fn read_bit_for_id(&self, interface: *mut u32, register: isize, id: InterruptId) -> bool {
        let (word_offset, bit_offset) = id_to_bit_offset(id);
        let reg = unsafe {
            let ptr = interface.offset(register).offset(word_offset);
            ptr.read_volatile()
        };
        reg & (1 << bit_offset) != 0
    }

    fn read_byte_for_id(&self, interface: *mut u32, register: isize, id: InterruptId) -> u8 {
        unsafe {
            let ptr = interface.offset(register);
            (ptr.offset((id / 4) as isize).read_volatile() >> ((id % 4) * 8)) as u8
        }
    }

    fn write_bit_for_id(&self, interface: *mut u32, register: isize, id: InterruptId, bit: bool) {
        let (word_offset, bit_offset) = id_to_bit_offset(id);
        unsafe {
            let ptr = interface.offset(register).offset(word_offset);
            log::debug!("writing GIC register bit 0x{:x} for id={id} (byte=0x{word_offset:x}, bit={bit_offset})", ptr as usize);
            ptr.write_volatile(1 << bit_offset);
        }
    }

    fn write_byte_for_id(&self, interface: *mut u32, register: isize, id: InterruptId, value: u8) {
        unsafe {
            let ptr = interface.offset(register).offset((id / 4) as isize);
            log::debug!("writing GIC register byte 0x{:x} for id={id}", ptr as usize);
            // TODO: this bitwise or is sus
            ptr.write_volatile(ptr.read_volatile() | ((value as u32) << ((id % 4) * 8)));
        }
    }
}

fn id_to_bit_offset(id: InterruptId) -> (isize, u32) {
    ((id / 32) as isize, (id % 32))
}

impl InterruptController for GenericInterruptController {
    fn is_enabled(&self, id: InterruptId) -> bool {
        self.read_bit_for_id(self.distributor_base, GICD_ISENABLER_N, id)
    }

    fn set_enable(&self, id: InterruptId, enabled: bool) {
        self.write_bit_for_id(
            self.distributor_base,
            if enabled {
                GICD_ISENABLER_N
            } else {
                GICD_ICENABLER_N
            },
            id,
            true,
        )
    }

    fn is_pending(&self, id: InterruptId) -> bool {
        self.read_bit_for_id(self.distributor_base, GICD_ISPENDR_N, id)
    }

    fn set_pending(&self, id: InterruptId, enabled: bool) {
        self.write_bit_for_id(
            self.distributor_base,
            if enabled {
                GICD_ISPENDR_N
            } else {
                GICD_ICPENDR_N
            },
            id,
            true,
        )
    }

    fn is_active(&self, id: InterruptId) -> bool {
        self.read_bit_for_id(self.distributor_base, GICD_ISACTIVER_N, id)
    }

    fn set_active(&self, id: InterruptId, enabled: bool) {
        self.write_bit_for_id(
            self.distributor_base,
            if enabled {
                GICD_ISACTIVER_N
            } else {
                GICD_ICACTIVER_N
            },
            id,
            true,
        )
    }

    fn priority(&self, id: InterruptId) -> u8 {
        self.read_byte_for_id(self.distributor_base, GICD_IPRIORITYR_N, id)
    }

    fn set_priority(&self, id: InterruptId, priority: u8) {
        self.write_byte_for_id(self.distributor_base, GICD_IPRIORITYR_N, id, priority)
    }

    fn target_cpu(&self, id: InterruptId) -> u8 {
        self.read_byte_for_id(self.distributor_base, GICD_ITARGETSR_N, id)
    }

    fn set_target_cpu(&self, id: InterruptId, target_cpu: u8) {
        self.write_byte_for_id(self.distributor_base, GICD_ITARGETSR_N, id, target_cpu)
    }

    fn config(&self, id: InterruptId) -> InterruptConfig {
        let (word_offset, bit_offset) = (
            (id / 16) as isize,
            BitIdx::new((id % 16) as u8 + 1).unwrap(),
        );
        unsafe {
            let ptr = self
                .distributor_base
                .offset(GICD_ICFGR_N)
                .offset(word_offset);
            let bp =
                BitPtr::<Const, u32, Lsb0>::new(ptr.as_ref().unwrap().into(), bit_offset).unwrap();
            match bp.read_volatile() {
                true => InterruptConfig::Edge,
                false => InterruptConfig::Level,
            }
        }
    }

    fn set_config(&self, id: InterruptId, config: InterruptConfig) {
        let (word_offset, bit_offset) = ((id / 16) as isize, BitIdx::new((id % 16) as u8).unwrap());
        unsafe {
            let ptr = self
                .distributor_base
                .offset(GICD_ICFGR_N)
                .offset(word_offset);
            let bp =
                BitPtr::<Mut, u32, Lsb0>::new(ptr.as_mut().unwrap().into(), bit_offset).unwrap();
            match config {
                InterruptConfig::Edge => {
                    bp.write_volatile(false);
                    bp.offset(1).write_volatile(true);
                }
                InterruptConfig::Level => {
                    bp.write_volatile(false);
                    bp.offset(1).write_volatile(false);
                }
            }
        }
    }

    fn ack_interrupt(&self) -> InterruptId {
        unsafe { self.cpu_base.offset(GICC_IAR).read_volatile() }
    }

    fn finish_interrupt(&self, id: InterruptId) {
        unsafe {
            self.cpu_base.offset(GICC_EOIR).write_volatile(id);
            self.cpu_base.offset(GICC_DIR).write_volatile(id);
        }
    }
}

//// Offsets for GIC registers in units of words (u32)

//// Distributor offsets
const GICD_CTLR: isize = 0x0000 >> 2;
const GICD_STATUSR: isize = 0x0010 >> 2;
const GICD_SETSPI_NSR: isize = 0x0040 >> 2;
const GICD_CLRSPI_NSR: isize = 0x0048 >> 2;
const GICD_SETSPI_SR: isize = 0x0050 >> 2;
const GICD_CLRSPI_SR: isize = 0x0058 >> 2;
const GICD_IGROUPR_N: isize = 0x0080 >> 2;
const GICD_ISENABLER_N: isize = 0x0100 >> 2;
const GICD_ICENABLER_N: isize = 0x0180 >> 2;
const GICD_ISPENDR_N: isize = 0x0200 >> 2;
const GICD_ICPENDR_N: isize = 0x0280 >> 2;
const GICD_ISACTIVER_N: isize = 0x0300 >> 2;
const GICD_ICACTIVER_N: isize = 0x0380 >> 2;
const GICD_IPRIORITYR_N: isize = 0x0400 >> 2;
const GICD_ITARGETSR_N: isize = 0x0800 >> 2;
const GICD_ICFGR_N: isize = 0x0c00 >> 2;
const GICD_IGRPMOD_N: isize = 0x0d00 >> 2;
const GICD_SGIR: isize = 0x0f00 >> 2;
const GICD_CPENDSGIR_N: isize = 0x0f10 >> 2;
const GICD_SPENDSGIR_N: isize = 0x0f20 >> 2;
const GICD_INMIR: isize = 0x0f80 >> 2;

//// CPU Interface offsets
const GICC_CTLR: isize = 0x000 >> 2;
const GICC_PMR: isize = 0x0004 >> 2;
const GICC_BPR: isize = 0x008 >> 2;
const GICC_IAR: isize = 0x000c >> 2;
const GICC_EOIR: isize = 0x0010 >> 2;
const GICC_RPR: isize = 0x0014 >> 2;
const GICC_HPPIR: isize = 0x0018 >> 2;
const GICC_ABPR: isize = 0x001c >> 2;
const GICC_AIAR: isize = 0x0020 >> 2;
const GICC_AEOIR: isize = 0x0024 >> 2;
const GICC_AHPPIR: isize = 0x0028 >> 2;
const GICC_STATUSR: isize = 0x002c >> 2;
const GICC_APR_N: isize = 0x00d0 >> 2;
const GICC_NSAPR_N: isize = 0x00e0 >> 2;
const GICC_IIDR: isize = 0x00fc >> 2;
const GICC_DIR: isize = 0x1000 >> 2;
