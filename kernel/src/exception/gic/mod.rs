//! Device driver for ARM's Generic Interrupt Controller (GIC).
//!
//! This driver supports GICv2 and also the v2m MSI extension.
//! It should be forward compatable with GICv3 as well, although the ITS MSI implementation is currently TODO.
use core::mem::size_of;

use alloc::boxed::Box;
use bitvec::{
    index::BitIdx,
    order::Lsb0,
    ptr::{BitPtr, Const, Mut},
};
use byteorder::{BigEndian, ByteOrder};

use crate::{
    dtb::{DeviceTree, MemRegionIter, StructureItem},
    memory::PhysicalAddress,
};

use super::{InterruptConfig, InterruptController, InterruptId, MsiDescriptor};

/* Possible GIC MSI implementations:
 * built-in MSI for GICv3 via writing SETSPI to generate an SPI with an InterruptId
 * built-in MSI for GICv3 via writing GICR_SETLPIR to generate an LPI (at the redistributor level, just like ITS)
 * use ITS in GICv3+, writing GITS_TRANSLATOR to generate an LPI
 * use v2m in GICv2, doing goodness knows what because I can't find any documentation, appears to generate an SPI
 * why are there four different ways to do this? it's not even that complicated of an idea, and
 * ideally it would be directly well supported by the base GIC - for goodness' sake it's required for PCIe!
 */

/// Support different MSI implementations for GIC using a trait object that handles `alloc_msi`
/// from [InterruptController].
trait MsiController {
    fn alloc_msi(&mut self) -> MsiDescriptor;
}

mod its;
mod v2m;

/// ARM Generic Interrupt Controller driver.
pub struct GenericInterruptController {
    distributor_base: *mut u32,
    cpu_base: *mut u32,
    msi_ctrl: Option<Box<dyn MsiController>>,
    dt_interrupt_cells: usize,
}

fn find_regs_for_node<'i, 'a: 'i>(
    dt: &'i mut impl Iterator<Item = StructureItem<'a>>,
) -> Option<MemRegionIter> {
    let mut regs = None;
    for j in dt.by_ref() {
        match j {
            StructureItem::Property {
                name: "reg", data, ..
            } => {
                regs = Some(MemRegionIter::for_data(data));
            }
            StructureItem::Property { .. } => {}
            StructureItem::EndNode => break,
            _ => unimplemented!("unexpected item in node {:?}", j),
        }
    }
    regs
}

impl GenericInterruptController {
    /// Initialize a GIC from information stored in the device tree, returning the
    /// [InterruptController] implementation if a GIC is found.
    pub fn in_device_tree(device_tree: &DeviceTree) -> Option<GenericInterruptController> {
        let mut gic_mem_regions = None;
        let mut found_node = false;
        let mut msi_ctrl = None;
        let mut intc_cells = 0;

        let mut dt = device_tree.iter_structure();
        while let Some(n) = dt.next() {
            match n {
                StructureItem::StartNode(name) if name.starts_with("intc") => {
                    found_node = true;
                }
                StructureItem::StartNode(name) if name.starts_with("its") => {
                    let regs = find_regs_for_node(&mut dt);
                    let its_base = regs.and_then(|mut i| i.next()).expect("found ITS base");
                    msi_ctrl = Some(Box::new(its::ItsMsiController::init(
                        PhysicalAddress(its_base.0 as usize),
                        its_base.1 as usize,
                    )) as Box<dyn MsiController>);
                }
                StructureItem::StartNode(name) if name.starts_with("v2m") => {
                    let regs = find_regs_for_node(&mut dt);
                    let v2m_base = regs.and_then(|mut i| i.next()).expect("found v2m base");
                    msi_ctrl = Some(Box::new(v2m::V2mMsiController::init(
                        PhysicalAddress(v2m_base.0 as usize),
                        v2m_base.1 as usize,
                    )) as Box<dyn MsiController>);
                }
                StructureItem::StartNode(_) if found_node => {
                    for j in dt.by_ref() {
                        if let StructureItem::EndNode = j {
                            break;
                        }
                    }
                }
                StructureItem::EndNode if found_node => break,
                StructureItem::Property {
                    name,
                    data,
                    std_interp: _,
                } if found_node => match name {
                    "reg" => {
                        gic_mem_regions = Some(MemRegionIter::for_data(data));
                    }
                    "#interrupt-cells" => intc_cells = BigEndian::read_u32(data),
                    _ => {}
                },
                _ => {}
            }
        }

        if !found_node || gic_mem_regions.is_none() {
            return None;
        }

        let mut gic_mem_regions = gic_mem_regions.take().unwrap();
        let distributor_base = PhysicalAddress(gic_mem_regions.next().unwrap().0 as usize);
        let cpu_base = PhysicalAddress(gic_mem_regions.next().unwrap().0 as usize);

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
                msi_ctrl,
                dt_interrupt_cells: intc_cells as usize,
            }
        };
        s.init_distributor();
        s.init_cpu_interface();
        Some(s)
    }

    fn init_distributor(&mut self) {
        unsafe {
            // enable distributor
            self.distributor_base
                .offset(dist_regs::CTLR)
                .write_volatile(0x1);

            // let typer = self.distributor_base.offset(dist_regs::TYPER).read_volatile();
            // log::info!("dist_regs::TYPER = {:032b}", typer);
            // if !typer.bit(16) {
            //     panic!("GIC does not support message-based interrupts");
            // } else {
            //     log::info!("GIC supports message-based interrupts");
            // }
        }
    }

    fn init_cpu_interface(&mut self) {
        unsafe {
            // enable cpu interface
            // make writes to cpu_regs::EOIR deactive interrupt (bits 9 and 10)
            // TODO: these bits don't seem to eliminate the need to write to cpu_regs::DIR anyways?
            self.cpu_base
                .offset(cpu_regs::CTLR)
                .write_volatile(0b0000_0110_0000_0001);
            // accept interrupts of any priority by setting the minimum priority
            // register to the lowest possible priority
            self.cpu_base.offset(cpu_regs::PMR).write_volatile(0xff);
            // disable group priority bits
            self.cpu_base.offset(cpu_regs::BPR).write_volatile(0x0);
        }
    }

    /// Get the bit flag of `register` for interrupt `id`.
    fn read_bit_for_id(&self, interface: *mut u32, register: isize, id: InterruptId) -> bool {
        let (word_offset, bit_offset) = id_to_bit_offset(id);
        let reg = unsafe {
            let ptr = interface.offset(register).offset(word_offset);
            ptr.read_volatile()
        };
        reg & (1 << bit_offset) != 0
    }

    /// Get the byte of `register` for interrupt `id`.
    fn read_byte_for_id(&self, interface: *mut u32, register: isize, id: InterruptId) -> u8 {
        unsafe {
            let ptr = interface.offset(register);
            (ptr.offset((id / 4) as isize).read_volatile() >> ((id % 4) * 8)) as u8
        }
    }

    /// Set the bit flag of `register` for the interrupt `id` high.
    fn write_bit_for_id(&self, interface: *mut u32, register: isize, id: InterruptId) {
        let (word_offset, bit_offset) = id_to_bit_offset(id);
        unsafe {
            let ptr = interface.offset(register).offset(word_offset);
            log::debug!("writing GIC register bit 0x{:x} for id={id} (byte=0x{word_offset:x}, bit={bit_offset})", ptr as usize);
            ptr.write_volatile(1 << bit_offset);
        }
    }

    /// Set the byte of `register` for interrupt `id`.
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
        self.read_bit_for_id(self.distributor_base, dist_regs::ISENABLER_N, id)
    }

    fn set_enable(&self, id: InterruptId, enabled: bool) {
        self.write_bit_for_id(
            self.distributor_base,
            if enabled {
                dist_regs::ISENABLER_N
            } else {
                dist_regs::ICENABLER_N
            },
            id,
        )
    }

    fn is_pending(&self, id: InterruptId) -> bool {
        self.read_bit_for_id(self.distributor_base, dist_regs::ISPENDR_N, id)
    }

    fn clear_pending(&self, id: InterruptId) {
        self.write_bit_for_id(self.distributor_base, dist_regs::ICPENDR_N, id)
    }

    fn is_active(&self, id: InterruptId) -> bool {
        self.read_bit_for_id(self.distributor_base, dist_regs::ISACTIVER_N, id)
    }

    fn set_active(&self, id: InterruptId, enabled: bool) {
        self.write_bit_for_id(
            self.distributor_base,
            if enabled {
                dist_regs::ISACTIVER_N
            } else {
                dist_regs::ICACTIVER_N
            },
            id,
        )
    }

    fn priority(&self, id: InterruptId) -> u8 {
        self.read_byte_for_id(self.distributor_base, dist_regs::IPRIORITYR_N, id)
    }

    fn set_priority(&self, id: InterruptId, priority: u8) {
        self.write_byte_for_id(self.distributor_base, dist_regs::IPRIORITYR_N, id, priority)
    }

    fn target_cpu(&self, id: InterruptId) -> u8 {
        self.read_byte_for_id(self.distributor_base, dist_regs::ITARGETSR_N, id)
    }

    fn set_target_cpu(&self, id: InterruptId, target_cpu: u8) {
        self.write_byte_for_id(
            self.distributor_base,
            dist_regs::ITARGETSR_N,
            id,
            target_cpu,
        )
    }

    fn config(&self, id: InterruptId) -> InterruptConfig {
        let (word_offset, bit_offset) = (
            (id / 16) as isize,
            BitIdx::new((id % 16) as u8 + 1).unwrap(),
        );
        unsafe {
            let ptr = self
                .distributor_base
                .offset(dist_regs::ICFGR_N)
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
                .offset(dist_regs::ICFGR_N)
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

    fn ack_interrupt(&self) -> Option<InterruptId> {
        let id = unsafe { self.cpu_base.offset(cpu_regs::IAR).read_volatile() };
        if id == INTID_NONE_PENDING {
            None
        } else {
            Some(id)
        }
    }

    fn finish_interrupt(&self, id: InterruptId) {
        unsafe {
            self.cpu_base.offset(cpu_regs::EOIR).write_volatile(id);
            self.cpu_base.offset(cpu_regs::DIR).write_volatile(id);
        }
    }

    fn msi_supported(&self) -> bool {
        self.msi_ctrl.is_some()
    }

    fn alloc_msi(&mut self) -> Option<MsiDescriptor> {
        self.msi_ctrl.as_mut().map(|c| c.alloc_msi())
    }

    fn device_tree_interrupt_spec_byte_size(&self) -> usize {
        self.dt_interrupt_cells * size_of::<u32>()
    }

    fn parse_interrupt_spec_from_device_tree(
        &self,
        data: &[u8],
    ) -> Option<(InterruptId, InterruptConfig)> {
        if data.len() != self.device_tree_interrupt_spec_byte_size() {
            return None;
        }
        let ty = BigEndian::read_u32(&data[0..4]);
        let irq = BigEndian::read_u32(&data[4..8]);
        let flags = BigEndian::read_u32(&data[8..12]);
        let offset = match ty {
            0 => {
                // SPI type interrupt
                assert!(irq <= 987);
                32
            }
            1 => {
                // PPI type interrupt
                assert!(irq <= 15);
                16
            }
            _ => panic!("unknown interrupt spec type: {ty} (irq:{irq} flags:{flags})"),
        };
        let cfg = match flags & 0xf {
            // low->high trigger
            1 => InterruptConfig::Edge,
            // high->low trigger
            2 => InterruptConfig::Edge,
            // active high
            3 => InterruptConfig::Level,
            // active low
            4 => InterruptConfig::Level,
            _ => panic!("unknown interrupt flag: {flags} (ty:{ty} irq:{irq})"),
        };
        let irq_offsetted = irq + offset;
        log::trace!("timer interrupt in device tree as {irq} with type={ty:x} and flags={flags:x} => {irq_offsetted}, {cfg:?}");
        Some((irq_offsetted, cfg))
    }
}

// Offsets for GIC registers in units of words (u32) //
// TODO: these should probably reside in modules or be made into a proper enum

/// Register offsets for the GIC distributor (relative to its base address).
#[allow(unused)]
mod dist_regs {
    pub const CTLR: isize = 0x0000 >> 2;
    pub const TYPER: isize = 0x0004 >> 2;
    pub const STATUSR: isize = 0x0010 >> 2;
    pub const SETSPI_NSR: isize = 0x0040 >> 2;
    pub const CLRSPI_NSR: isize = 0x0048 >> 2;
    pub const SETSPI_SR: isize = 0x0050 >> 2;
    pub const CLRSPI_SR: isize = 0x0058 >> 2;
    pub const IGROUPR_N: isize = 0x0080 >> 2;
    pub const ISENABLER_N: isize = 0x0100 >> 2;
    pub const ICENABLER_N: isize = 0x0180 >> 2;
    pub const ISPENDR_N: isize = 0x0200 >> 2;
    pub const ICPENDR_N: isize = 0x0280 >> 2;
    pub const ISACTIVER_N: isize = 0x0300 >> 2;
    pub const ICACTIVER_N: isize = 0x0380 >> 2;
    pub const IPRIORITYR_N: isize = 0x0400 >> 2;
    pub const ITARGETSR_N: isize = 0x0800 >> 2;
    pub const ICFGR_N: isize = 0x0c00 >> 2;
    pub const IGRPMOD_N: isize = 0x0d00 >> 2;
    pub const SGIR: isize = 0x0f00 >> 2;
    pub const CPENDSGIR_N: isize = 0x0f10 >> 2;
    pub const SPENDSGIR_N: isize = 0x0f20 >> 2;
    pub const INMIR: isize = 0x0f80 >> 2;
}

/// Register offsets for the GIC CPU interface (relative to its base address).
#[allow(unused)]
mod cpu_regs {
    pub const CTLR: isize = 0x000 >> 2;
    pub const PMR: isize = 0x0004 >> 2;
    pub const BPR: isize = 0x008 >> 2;
    pub const IAR: isize = 0x000c >> 2;
    pub const EOIR: isize = 0x0010 >> 2;
    pub const RPR: isize = 0x0014 >> 2;
    pub const HPPIR: isize = 0x0018 >> 2;
    pub const ABPR: isize = 0x001c >> 2;
    pub const AIAR: isize = 0x0020 >> 2;
    pub const AEOIR: isize = 0x0024 >> 2;
    pub const AHPPIR: isize = 0x0028 >> 2;
    pub const STATUSR: isize = 0x002c >> 2;
    pub const APR_N: isize = 0x00d0 >> 2;
    pub const NSAPR_N: isize = 0x00e0 >> 2;
    pub const IIDR: isize = 0x00fc >> 2;
    pub const DIR: isize = 0x1000 >> 2;
}

// Special InterruptIds //
const INTID_NONE_PENDING: InterruptId = 1023;
