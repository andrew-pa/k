use core::cell::OnceCell;

use alloc::boxed::Box;
use bitfield::Bit;
use byteorder::{BigEndian, ByteOrder, LittleEndian};
use derive_more::Display;
use hashbrown::HashMap;

use crate::{
    dtb::{DeviceTree, MemRegionIter},
    memory::{self, PhysicalAddress, VirtualAddress},
    CHashMapG,
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

#[derive(Debug)]
pub struct BaseAddresses {
    pub mmio: PhysicalAddress,
    pub mmio_size: usize,
    pub pio: PhysicalAddress,
    pub pio_size: usize,
    pub ecam: PhysicalAddress,
    pub ecam_size: usize,
}

impl BaseAddresses {
    fn from_dt(node: &HostCtrlDeviceTreeNode) -> Self {
        // TODO: right now this only supports deserializing the format given to us by QEMU
        assert_eq!(node.size_cells, 2);
        assert_eq!(node.address_cells, 3);
        assert!(node.reg.len() == 16);
        log::debug!("ecam = {:x?}", node.reg);
        let mut offset = 0;
        let ecam_base = BigEndian::read_u64(node.reg);
        offset += 8;
        let ecam_size = BigEndian::read_u64(&node.reg[offset..]) as usize;

        offset = 3 * 4;

        let pio_base = BigEndian::read_u64(&node.ranges[offset..]);
        offset += 8;
        let pio_size = BigEndian::read_u64(&node.ranges[offset..]) as usize;
        offset += 8;

        offset += 12;
        let mmio_base = BigEndian::read_u64(&node.ranges[offset..]);
        offset += 8;
        let mmio_size = BigEndian::read_u64(&node.ranges[offset..]) as usize;

        Self {
            mmio: PhysicalAddress(mmio_base as usize),
            mmio_size,
            pio: PhysicalAddress(pio_base as usize),
            pio_size,
            ecam: PhysicalAddress(ecam_base as usize),
            ecam_size,
        }
    }
}

pub const PCI_ECAM_START: VirtualAddress = VirtualAddress(0xffff_0001_0000_0000);
pub const PCI_MMIO_START: VirtualAddress = VirtualAddress(0xffff_0002_0000_0000);

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct DeviceId {
    bus: u8,
    device: u8,
    function: u8,
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
    unsafe fn for_device(id: DeviceId) -> ConfigBlock {
        let addr = PCI_ECAM_START.offset(
            ((id.bus as isize) * 256 + (id.device as isize) * 8 + (id.function as isize)) * 4096,
        );

        ConfigBlock { p: addr.as_ptr() }
    }

    fn read_word(&self, offset: isize) -> u16 {
        unsafe { (self.p.offset(offset) as *mut u16).read_volatile() }
    }

    pub fn vendor_id(&self) -> u16 {
        self.read_word(HEADER_VENDOR_ID)
    }

    pub fn device_id(&self) -> u16 {
        self.read_word(HEADER_DEVICE_ID)
    }

    pub fn class(&self) -> u32 {
        unsafe { (self.p.offset(HEADER_CLASS_CODE) as *mut u32).read_volatile() }
    }

    pub fn multifunction(&self) -> bool {
        unsafe { self.p.offset(HEADER_TYPE_ID).read_volatile().bit(7) }
    }

    pub fn header(&self) -> ConfigHeader {
        let header_type = unsafe { self.p.offset(HEADER_TYPE_ID).read_volatile() };
        match header_type & 0x7f {
            0 => ConfigHeader::Type0(Type0ConfigHeader { p: self.p }),
            x => todo!("unknown header type {x:x}"),
        }
    }
}

pub mod msix;

#[derive(Debug)]
pub enum CapabilityBlock {
    Msi,
    MsiX(msix::MsiXCapability),
    Unknown { id: u8 },
}

const CAPABILITY_ID_MSI: u8 = 0x05;
const CAPABILITY_ID_MSIX: u8 = 0x11;

impl CapabilityBlock {
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

impl<'d> Iterator for CapabilityIter {
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

pub struct Type0ConfigHeader {
    p: *mut u8,
}

pub enum ConfigHeader {
    Type0(Type0ConfigHeader),
}

impl ConfigHeader {
    pub fn as_type0(&self) -> Option<&Type0ConfigHeader> {
        if let Self::Type0(v) = self {
            Some(v)
        } else {
            None
        }
    }
}

impl Type0ConfigHeader {
    pub fn base_addresses(&self) -> &[u32] {
        unsafe { core::slice::from_raw_parts(self.p.offset(0x10) as *const u32, 5) }
    }

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

pub trait DeviceDriver {}

static mut DEVICES: OnceCell<CHashMapG<DeviceId, Box<dyn DeviceDriver>>> = OnceCell::new();

#[derive(Debug, Display)]
pub enum Error {
    Other(&'static str),
}

pub type DriverInitFn =
    fn(DeviceId, &ConfigBlock, &BaseAddresses) -> Result<Box<dyn DeviceDriver>, Error>;

pub fn init(dt: &DeviceTree, driver_registry: &HashMap<u32, DriverInitFn>) {
    assert!(
        crate::exception::interrupt_controller().msi_supported(),
        "MSIs must be supported for PCIe"
    );

    let node = HostCtrlDeviceTreeNode::find_in_tree(dt).expect("find PCIe host in DeviceTree");
    let base = BaseAddresses::from_dt(&node);
    log::debug!("PCIe = {base:#x?}");

    // map ECAM registers into memory
    {
        let mut pt = memory::paging::kernel_table();
        pt.map_range(
            base.ecam,
            PCI_ECAM_START,
            base.ecam_size / memory::PAGE_SIZE,
            true,
            &memory::paging::PageTableEntryOptions::default(),
        )
        .expect("map ecam range");

        pt.map_range(
            base.mmio,
            PCI_MMIO_START,
            base.mmio_size / memory::PAGE_SIZE,
            true,
            &memory::paging::PageTableEntryOptions::default(),
        )
        .expect("map mmio range");
    }

    // scan the bus for devices and initialize drivers
    let devices = CHashMapG::new();
    for bus in 0..=255 {
        for device in 0..32 {
            let addr = DeviceId::new(bus, device, 0);
            let cfg = unsafe { ConfigBlock::for_device(addr) };
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
                        Ok(dd) => {
                            devices.insert(addr, dd);
                        }
                        Err(e) => {
                            log::error!("failed to initalize PCIe driver for device at {addr}: {e} (vendor={:x}, device={:x}, class={:x}", cfg.vendor_id(), cfg.device_id(), cfg.class())
                        }
                    }
                }
            }
        }
    }

    unsafe {
        DEVICES.set(devices).ok().expect("init PCIe once");
    }
}
