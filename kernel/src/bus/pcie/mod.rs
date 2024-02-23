//! PCIe bus device driver.
//!
//! This module provides basic PCIe data structures, bus initialization and device discovery.
use core::{cell::OnceCell, marker::PhantomData};

use alloc::{boxed::Box, string::String};
use bitfield::Bit;
use byteorder::{BigEndian, ByteOrder, LittleEndian};
use derive_more::Display;
use hashbrown::HashMap;
use snafu::{ResultExt, Snafu};

use crate::{
    dtb::{DeviceTree, MemRegionIter},
    memory::{self, PhysicalAddress, VirtualAddress, PAGE_SIZE},
};

#[derive(Debug)]
struct HostCtrlDeviceTreeNode<'dt> {
    size_cells: u32,
    address_cells: u32,
    interrupt_cells: u32,
    ranges: &'dt [u8],
    reg: &'dt [u8],
    interrupt_map: &'dt [u8],
}

impl<'dt> HostCtrlDeviceTreeNode<'dt> {
    fn find_in_tree(dt: &'dt DeviceTree) -> Option<Self> {
        let mut size_cells = None;
        let mut address_cells = None;
        let mut interrupt_cells = None;
        let mut ranges = None;
        let mut reg = None;
        let mut interrupt_map = None;

        dt.process_properties_for_node("pcie", |name, data, std_interp| match name {
            "#size-cells" => {
                size_cells = Some(match std_interp.unwrap() {
                    crate::dtb::StandardProperty::SizeCells(s) => s,
                    _ => unreachable!(),
                })
            }
            "#address-cells" => {
                address_cells = Some(match std_interp.unwrap() {
                    crate::dtb::StandardProperty::AddressCells(s) => s,
                    _ => unreachable!(),
                })
            }
            "#interrupt-cells" => interrupt_cells = Some(BigEndian::read_u32(data)),
            "ranges" => ranges = Some(data),
            "reg" => reg = Some(data),
            "interrupt-map" => interrupt_map = Some(data),
            _ => {}
        });

        size_cells.and_then(|size_cells| {
            address_cells.and_then(|address_cells| {
                interrupt_cells.and_then(|interrupt_cells| {
                    ranges.and_then(|ranges| {
                        reg.and_then(|reg| {
                            interrupt_map.map(|interrupt_map| Self {
                                size_cells,
                                address_cells,
                                interrupt_cells,
                                ranges,
                                reg,
                                interrupt_map,
                            })
                        })
                    })
                })
            })
        })
    }
}

/// Location and size of the physical and virtual regions of memory that are mapped to the PCIe controller.
#[derive(Debug)]
pub struct BaseAddresses {
    /// Physical base address of the MMIO (memory mapped IO) region.
    pub mmio_phy: PhysicalAddress,
    /// Virtual base address of the MMIO (memory mapped IO) region.
    pub mmio_vrt: VirtualAddress,
    /// Size of the MMIO region in bytes.
    pub mmio_size: usize,
    /// Base address of the PIO (Programmed IO) region.
    pub pio: PhysicalAddress,
    /// Size of the PIO region in bytes.
    pub pio_size: usize,
    /// Physical base address of the ECAM (Enhanced Configuration Access Mechanism) region aka the configuration space.
    pub ecam_phy: PhysicalAddress,
    /// Virtual base address of the ECAM (Enhanced Configuration Access Mechanism) region aka the configuration space.
    pub ecam_vrt: VirtualAddress,
    /// Size of the ECAM region in bytes.
    pub ecam_size: usize,
}

impl BaseAddresses {
    /// Calculate the PCIe base addresses from the values in the host controller device tree node,
    /// and then map them into virtual memory.
    fn from_dt(node: &HostCtrlDeviceTreeNode) -> Result<Self, Error> {
        // TODO: right now this only supports deserializing the format given to us by QEMU
        assert_eq!(node.size_cells, 2);
        assert_eq!(node.address_cells, 3);
        assert!(node.reg.len() == 16);
        log::debug!("ecam = {:x?}", node.reg);
        let mut offset = 0;
        let ecam_base = PhysicalAddress(BigEndian::read_u64(node.reg) as usize);
        offset += 8;
        let ecam_size = BigEndian::read_u64(&node.reg[offset..]) as usize;

        offset = 3 * 4;

        let pio_base = BigEndian::read_u64(&node.ranges[offset..]);
        offset += 8;
        let pio_size = BigEndian::read_u64(&node.ranges[offset..]) as usize;
        offset += 8;

        offset += 12;
        let mmio_base = PhysicalAddress(BigEndian::read_u64(&node.ranges[offset..]) as usize);
        offset += 8;
        let mmio_size = BigEndian::read_u64(&node.ranges[offset..]) as usize;

        let (mmio_vrt, ecam_vrt) = {
            let mut vaa = memory::virtual_address_allocator();
            let ecam_vrt = vaa
                .alloc(ecam_size.div_ceil(PAGE_SIZE))
                .context(error::MemorySnafu)?;
            let mmio_vrt = vaa
                .alloc(mmio_size.div_ceil(PAGE_SIZE))
                .context(error::MemorySnafu)?;
            (mmio_vrt, ecam_vrt)
        };

        // map registers into kernel virtual address space
        {
            let mut pt = memory::paging::kernel_table();
            pt.map_range(
                ecam_base,
                ecam_vrt,
                ecam_size / memory::PAGE_SIZE,
                true,
                &memory::paging::PageTableEntryOptions::default(),
            )
            .context(error::MappingSnafu)?;

            pt.map_range(
                mmio_base,
                mmio_vrt,
                mmio_size / memory::PAGE_SIZE,
                true,
                &memory::paging::PageTableEntryOptions::default(),
            )
            .context(error::MappingSnafu)?;
        }

        Ok(Self {
            mmio_phy: mmio_base,
            mmio_vrt,
            mmio_size,
            pio: PhysicalAddress(pio_base as usize),
            pio_size,
            ecam_phy: ecam_base,
            ecam_vrt,
            ecam_size,
        })
    }
}

/// The identifier of a device on the bus.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct DeviceId {
    /// The bus index.
    pub bus: u8,
    /// The device index on the bus.
    pub device: u8,
    /// The function index on the device.
    pub function: u8,
}

impl DeviceId {
    fn new(bus: u8, device: u8, function: u8) -> Self {
        assert!(device < 32);
        assert!(function < 8);
        Self {
            bus,
            device,
            function,
        }
    }
}

impl core::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "pci@{:x}:{:x}:{:x}",
            self.bus, self.device, self.function
        )
    }
}

/// A PCIe configuration block in configuration space.
pub struct ConfigBlock {
    p: *mut u8,
}

// offsets in units of u8s from the start of configuration space
const HEADER_VENDOR_ID: isize = 0x0;
const HEADER_DEVICE_ID: isize = 0x2;
const HEADER_COMMAND: isize = 0x4;
const HEADER_STATUS: isize = 0x6;
const HEADER_CLASS_CODE: isize = 0x8;
const HEADER_TYPE_ID: isize = 0xe;

impl ConfigBlock {
    /// Create a configuration block by computing its base address from the id of the device and
    /// the known base address in the kernel address space of the ECAM region.
    unsafe fn for_device(base: &BaseAddresses, id: DeviceId) -> ConfigBlock {
        let addr = base.ecam_vrt.add(
            ((id.bus as usize) * 256 + (id.device as usize) * 8 + (id.function as usize)) * 4096,
        );

        ConfigBlock { p: addr.as_ptr() }
    }

    fn read_word(&self, offset: isize) -> u16 {
        unsafe { (self.p.offset(offset) as *mut u16).read_volatile() }
    }

    /// Reads the PCIe vendor ID of the device.
    pub fn vendor_id(&self) -> u16 {
        self.read_word(HEADER_VENDOR_ID)
    }

    /// Reads the device ID.
    pub fn device_id(&self) -> u16 {
        self.read_word(HEADER_DEVICE_ID)
    }

    /// Reads the device PCIe class.
    pub fn class(&self) -> u32 {
        unsafe { (self.p.offset(HEADER_CLASS_CODE) as *mut u32).read_volatile() }
    }

    /// Returns true if the device reports that it is multifunction.
    pub fn multifunction(&self) -> bool {
        unsafe { self.p.offset(HEADER_TYPE_ID).read_volatile().bit(7) }
    }

    /// Get the configuration header for the block, which refers to the same location in memory.
    pub fn header(&self) -> ConfigHeader {
        let header_type = unsafe { self.p.offset(HEADER_TYPE_ID).read_volatile() };
        match header_type & 0x7f {
            0 => ConfigHeader::Type0(Type0ConfigHeader {
                p: self.p,
                lt: PhantomData,
            }),
            x => todo!("unknown header type {x:x}"),
        }
    }
}

pub mod msix;

/// A device capability block.
#[derive(Debug)]
pub enum CapabilityBlock {
    /// The MSI (Message Signaled Interrupts) capability block.
    Msi,
    /// The MSI-X (Message Signaled Interrupts eXtended) capability block.
    MsiX(msix::MsiXCapability),
    /// An unknown device capability.
    Unknown { id: u8 },
}

const CAPABILITY_ID_MSI: u8 = 0x05;
const CAPABILITY_ID_MSIX: u8 = 0x11;

impl CapabilityBlock {
    /// Identify the capability block at some address in configuration space.
    fn at_address(block: *mut u8) -> CapabilityBlock {
        match unsafe { block.read_volatile() } {
            CAPABILITY_ID_MSI => CapabilityBlock::Msi,
            CAPABILITY_ID_MSIX => CapabilityBlock::MsiX(msix::MsiXCapability::at_address(block)),
            id => CapabilityBlock::Unknown { id },
        }
    }
}

struct CapabilityIter {
    p: *mut u8,
    next_block: u8,
}

impl Iterator for CapabilityIter {
    type Item = CapabilityBlock;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next_block == 0 {
            None
        } else {
            let c = self.next_block;
            unsafe {
                let block = self.p.offset(c as isize);
                self.next_block = block.offset(1).read_volatile() & 0b1111_1100;
                Some(CapabilityBlock::at_address(block))
            }
        }
    }
}

/// A PCIe configuration header for a device.
#[non_exhaustive]
pub enum ConfigHeader<'a> {
    Type0(Type0ConfigHeader<'a>),
}

impl<'a> ConfigHeader<'a> {
    /// If the header is type 0, returns a reference to the type 0 header.
    pub fn as_type0(&self) -> Option<&Type0ConfigHeader> {
        #[allow(irrefutable_let_patterns)]
        if let Self::Type0(v) = self {
            Some(v)
        } else {
            None
        }
    }
}

/// A PCIe Type 0 configuration header (the most common).
pub struct Type0ConfigHeader<'a> {
    p: *mut u8,
    lt: PhantomData<&'a ConfigBlock>,
}

impl Type0ConfigHeader<'_> {
    /// Get the base address list for the device.
    pub fn base_addresses(&self) -> &[u32] {
        unsafe { core::slice::from_raw_parts(self.p.offset(0x10) as *const u32, 5) }
    }

    /// Read the base address present at a particular index in the base address array.
    ///
    /// If the address is 64-bit, the high word will also be read from the array.
    /// 32-bit base addresses are converted to 64-bit.
    pub fn base_address(&self, index: usize) -> u64 {
        let bars = self.base_addresses();
        let barl = bars[index];
        if barl.bit(2) {
            let barh = bars[index + 1];
            (barl & 0xffff_fff0) as u64 | ((barh as u64) << 32)
        } else {
            (barl & 0xffff_fff0) as u64
        }
    }

    /// Iterate over the capability blocks present for this device.
    pub fn capabilities(&self) -> impl Iterator<Item = CapabilityBlock> + '_ {
        let status = unsafe { self.p.offset(0x6).read_volatile() };
        CapabilityIter {
            p: self.p,
            next_block: if status.bit(4) {
                unsafe { self.p.offset(0x34).read_volatile() & 0b1111_1100 }
            } else {
                0
            },
        }
    }
}

/// Errors arising from the PCIe bus.
#[derive(Debug, Snafu)]
#[snafu(module, visibility(pub))]
pub enum Error {
    Memory {
        source: memory::MemoryError,
    },
    Mapping {
        source: memory::paging::MapError,
    },
    Registry {
        source: crate::registry::RegistryError,
    },
}

/// The type of functions that can be registered to handle the discovery of a device on the bus.
pub type DriverInitFn = fn(DeviceId, &ConfigBlock, &BaseAddresses) -> Result<(), Error>;

/// Initialize the PCIe bus using the device tree to discover the host controller. Drivers in the
/// registry will be invoked for any discovered devices on the bus.
///
/// This function assumes there is only one PCIe host controller in the system.
// TODO: return errors instead of panicking?
pub fn init(dt: &DeviceTree, driver_registry: &HashMap<u32, DriverInitFn>) {
    assert!(
        crate::exception::interrupt_controller().msi_supported(),
        "MSIs must be supported for PCIe"
    );

    let node = HostCtrlDeviceTreeNode::find_in_tree(dt).expect("find PCIe host in DeviceTree");
    let base = BaseAddresses::from_dt(&node).expect("locate and map PCIe base addresses");
    log::debug!("PCIe = {base:x?}");

    // scan the bus for devices and initialize drivers
    for bus in 0..=255 {
        for device in 0..32 {
            let addr = DeviceId::new(bus, device, 0);
            let cfg = unsafe { ConfigBlock::for_device(&base, addr) };
            if cfg.vendor_id() != 0xffff {
                log::debug!(
                    "addr = {addr}, vendor = {:x}, device = {:x}, class = {:08x}",
                    cfg.vendor_id(),
                    cfg.device_id(),
                    cfg.class()
                );
                let ConfigHeader::Type0(hdr) = cfg.header();
                for (i, bar) in hdr.base_addresses().iter().enumerate() {
                    log::debug!("\tbar #{i} = {bar:x}");
                }
                for cap in hdr.capabilities() {
                    log::debug!("\tcap {cap:?}");
                }
                if let Some(driver_init) = driver_registry.get(&(cfg.class() & !0xff)) {
                    match driver_init(addr, &cfg, &base) {
                        Ok(()) => {}
                        Err(e) => {
                            log::error!("failed to initalize PCIe driver for device at {addr}: {e} (vendor={:x}, device={:x}, class={:x}", cfg.vendor_id(), cfg.device_id(), cfg.class())
                        }
                    }
                }
            }
        }
    }
}
