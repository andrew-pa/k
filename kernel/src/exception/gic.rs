use core::ffi::CStr;

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
                StructureItem::Property { name, data } if found_node => match name {
                    "reg" => {
                        let mut r = MemRegionIter::for_data(data);
                        distributor_base = r.next();
                        cpu_base = r.next();
                    }
                    "compatible" => {
                        if let Some(s) = CStr::from_bytes_with_nul(data).ok() {
                            log::trace!("interrupt controller compatible = {:?}", s);
                        }
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
            self.cpu_base.offset(GICC_CTLR).write_volatile(0x1);
            // accept interrupts of any priority by setting the minimum priority
            // register to the lowest possible priority
            self.cpu_base.offset(GICC_PMR).write_volatile(0xff);
            // disable group priority bits
            self.cpu_base.offset(GICC_BPR).write_volatile(0x0);
        }
    }

    fn read_bit_for_id(&self, register: isize, id: InterruptId) -> bool {
        let (byte_offset, bit_offset) = id_to_bit_offset(id);
        unsafe {
            let ptr = self.distributor_base.offset(register).offset(byte_offset);
            let bp =
                BitPtr::<Const, u32, Lsb0>::new(ptr.as_ref().unwrap().into(), bit_offset).unwrap();
            bp.read_volatile()
        }
    }

    fn read_byte_for_id(&self, register: isize, id: InterruptId) -> u8 {
        unsafe {
            let ptr = self.distributor_base.offset(register) as *mut u8;
            ptr.offset(id as isize).read_volatile()
        }
    }

    fn write_bit_for_id(&self, register: isize, id: InterruptId, bit: bool) {
        let (byte_offset, bit_offset) = id_to_bit_offset(id);
        unsafe {
            let ptr = self.distributor_base.offset(register).offset(byte_offset);
            let bp =
                BitPtr::<Mut, u32, Lsb0>::new(ptr.as_mut().unwrap().into(), bit_offset).unwrap();
            bp.write_volatile(bit);
        }
    }

    fn write_byte_for_id(&self, register: isize, id: InterruptId, value: u8) {
        unsafe {
            let ptr = self.distributor_base.offset(register) as *mut u8;
            ptr.offset(id as isize).write_volatile(value);
        }
    }
}

fn id_to_bit_offset(id: InterruptId) -> (isize, BitIdx<u32>) {
    (
        (id / 32) as isize,
        BitIdx::<u32>::new((id % 32) as u8).unwrap(),
    )
}

impl InterruptController for GenericInterruptController {
    fn is_enabled(&self, id: InterruptId) -> bool {
        self.read_bit_for_id(GICD_ISENABLER_N, id)
    }

    fn set_enable(&self, id: InterruptId, enabled: bool) {
        self.write_bit_for_id(
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
        self.read_bit_for_id(GICD_ISPENDR_N, id)
    }

    fn set_pending(&self, id: InterruptId, enabled: bool) {
        self.write_bit_for_id(
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
        self.read_bit_for_id(GICD_ISACTIVER_N, id)
    }

    fn set_active(&self, id: InterruptId, enabled: bool) {
        self.write_bit_for_id(
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
        self.read_byte_for_id(GICD_IPRIORITYR_N, id)
    }

    fn set_priority(&self, id: InterruptId, priority: u8) {
        self.write_byte_for_id(GICD_IPRIORITYR_N, id, priority)
    }

    fn target_cpu(&self, id: InterruptId) -> u8 {
        self.read_byte_for_id(GICD_ITARGETSR_N, id)
    }

    fn set_target_cpu(&self, id: InterruptId, target_cpu: u8) {
        self.write_byte_for_id(GICD_ITARGETSR_N, id, target_cpu)
    }

    fn config(&self, id: InterruptId) -> InterruptConfig {
        let (byte_offset, bit_offset) = (
            (id / 16) as isize,
            BitIdx::new((id % 16) as u8 + 1).unwrap(),
        );
        unsafe {
            let ptr = self
                .distributor_base
                .offset(GICD_ICFGR_N)
                .offset(byte_offset);
            let bp =
                BitPtr::<Const, u32, Lsb0>::new(ptr.as_ref().unwrap().into(), bit_offset).unwrap();
            match bp.read_volatile() {
                true => InterruptConfig::Edge,
                false => InterruptConfig::Level,
            }
        }
    }

    fn set_config(&self, id: InterruptId, config: InterruptConfig) {
        let (byte_offset, bit_offset) = ((id / 16) as isize, BitIdx::new((id % 16) as u8).unwrap());
        unsafe {
            let ptr = self
                .distributor_base
                .offset(GICD_ICFGR_N)
                .offset(byte_offset);
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
}

//// Distributor offsets
const GICD_CTLR: isize = 0x0000;
const GICD_STATUSR: isize = 0x0010;
const GICD_SETSPI_NSR: isize = 0x0040;
const GICD_CLRSPI_NSR: isize = 0x0048;
const GICD_SETSPI_SR: isize = 0x0050;
const GICD_CLRSPI_SR: isize = 0x0058;
const GICD_IGROUPR_N: isize = 0x0080;
const GICD_ISENABLER_N: isize = 0x0100;
const GICD_ICENABLER_N: isize = 0x0180;
const GICD_ISPENDR_N: isize = 0x0200;
const GICD_ICPENDR_N: isize = 0x0280;
const GICD_ISACTIVER_N: isize = 0x0300;
const GICD_ICACTIVER_N: isize = 0x0380;
const GICD_IPRIORITYR_N: isize = 0x0400;
const GICD_ITARGETSR_N: isize = 0x0800;
const GICD_ICFGR_N: isize = 0x0c00;
const GICD_IGRPMOD_N: isize = 0x0d00;
const GICD_SGIR: isize = 0x0f00;
const GICD_CPENDSGIR_N: isize = 0x0f10;
const GICD_SPENDSGIR_N: isize = 0x0f20;
const GICD_INMIR: isize = 0x0f80;

//// CPU Interface offsets
const GICC_CTLR: isize = 0x000;
const GICC_PMR: isize = 0x0004;
const GICC_BPR: isize = 0x008;
const GICC_IAR: isize = 0x000c;
const GICC_EOIR: isize = 0x0010;
const GICC_RPR: isize = 0x0014;
const GICC_HPPIR: isize = 0x0018;
const GICC_ABPR: isize = 0x001c;
const GICC_AIAR: isize = 0x0020;
const GICC_AEOIR: isize = 0x0024;
const GICC_AHPPIR: isize = 0x0028;
const GICC_STATUSR: isize = 0x002c;
const GICC_APR_N: isize = 0x00d0;
const GICC_NSAPR_N: isize = 0x00e0;
const GICC_IIDR: isize = 0x00fc;
const GICC_DIR: isize = 0x1000;
