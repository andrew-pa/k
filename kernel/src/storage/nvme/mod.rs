use alloc::boxed::Box;

use crate::bus::pcie;

pub struct PcieDriver {}

impl pcie::DeviceDriver for PcieDriver {}

pub fn init_nvme_over_pcie(
    addr: pcie::DeviceId,
    config: &pcie::ConfigBlock,
    base: &pcie::BaseAddresses,
) -> Result<Box<dyn pcie::DeviceDriver>, pcie::Error> {
    log::info!("initializing NVMe over PCIe at {addr}");
    log::info!(
        "vendor = {:x}, device id = {:x}",
        config.vendor_id(),
        config.device_id()
    );

    let hdr = config.header();
    let hdr = hdr.as_type0().unwrap();

    let base_addresses = hdr.base_addresses();
    let barl = base_addresses[0];
    let barh = base_addresses[1];
    let base_address = (barl & 0xffff_fff0) as u64 | ((barh as u64) << 32);

    log::debug!(
        "NVMe base address = {:x} ({:x} <? {:x})",
        base_address,
        base_address as usize + base.mmio.0,
        base.mmio_size
    );

    let base_vaddress = pcie::PCI_MMIO_START.offset((base_address as usize - base.mmio.0) as isize);
    log::debug!("vbar = {}", base_vaddress);

    let nvme_version = unsafe { base_vaddress.as_ptr::<u64>().offset(1).read_volatile() };

    log::info!("device supports NVMe version {nvme_version:x}");

    let mut msix_table = None;

    for cap in hdr.capabilities() {
        match cap {
            pcie::CapabilityBlock::MsiX(msix) => {
                log::info!("NVMe device uses MSI-X: {msix:?}");
                msix_table = Some(pcie::msix::MsiXTable::from_config(base_addresses, &msix));
                break;
            }
            _ => {}
        }
    }

    Ok(Box::new(PcieDriver {}))
}
