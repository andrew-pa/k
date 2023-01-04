use alloc::boxed::Box;

use crate::bus::pcie;

pub struct PcieDriver {}

impl pcie::DeviceDriver for PcieDriver {}

pub fn init_nvme_over_pcie(
    addr: pcie::DeviceId,
    config: &pcie::ConfigBlock,
) -> Result<Box<dyn pcie::DeviceDriver>, pcie::Error> {
    log::info!("initializing NVMe over PCIe at {addr}");
    log::info!(
        "vendor = {:x}, device id = {:x}",
        config.vendor_id(),
        config.device_id()
    );
    Ok(Box::new(PcieDriver {}))
}
