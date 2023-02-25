use crate::{
    bus::pcie,
    memory::{physical_memory_allocator, PhysicalAddress, VirtualAddress, PAGE_SIZE},
};
use alloc::boxed::Box;
use bitfield::{bitfield, Bit};

mod queue;

// PCIe Register offsets (in bytes)
const REG_CAP: isize = 0x00;
const REG_VS: isize = 0x08;
const REG_CC: isize = 0x14;
const REG_CSTS: isize = 0x1c;
const REG_AQA: isize = 0x24;
const REG_ASQ: isize = 0x28;
const REG_ACQ: isize = 0x30;

const SUBMISSION_ENTRY_SIZE: usize = 64; //bytes
const COMPLETION_ENTRY_SIZE: usize = 16; //bytes (at least)

bitfield! {
    struct ControllerCapabilities(u64);
    impl Debug;
    u8;
    doorbell_stride, _: 35, 32;
}

bitfield! {
    struct ControllerConfigReg(u32);
    impl Debug;
    io_completion_queue_entry_size, set_io_completion_queue_entry_size: 23, 20;
    io_submission_queue_entry_size, set_io_submission_queue_entry_size: 19, 16;
    shutdown_notification, set_shutdown_notification: 15, 14;
    arbitration_mechanism, set_arbitration_mechanism: 13, 11;
    memory_page_size, set_memory_page_size: 10, 7;
    io_command_set_selected, set_io_command_set_selected: 6, 4;
    enable, set_enable: 0;
}

bitfield! {
    struct AdminQueueAttributes(u32);
    impl Debug;
    u16;
    completion_queue_size, set_completion_queue_size: 27, 16;
    submission_queue_size, set_submission_queue_size: 11, 0;
}

impl Copy for AdminQueueAttributes {}
impl Clone for AdminQueueAttributes {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

pub struct PcieDriver {}

impl pcie::DeviceDriver for PcieDriver {}

fn busy_wait_for_ready_bit(csts: *mut u8, value: bool) {
    unsafe {
        loop {
            if csts.read_volatile().bit(0) == value {
                break;
            }
        }
    }
}

impl PcieDriver {
    fn configure(base_address: VirtualAddress) -> Result<PcieDriver, crate::memory::MemoryError> {
        let cap: ControllerCapabilities = unsafe {
            base_address
                .offset(REG_CAP)
                .as_ptr::<ControllerCapabilities>()
                .read_volatile()
        };
        log::debug!("CAP = {:?}", cap);

        // reset the controller
        let cc: *mut ControllerConfigReg = base_address.offset(REG_CC).as_ptr();
        unsafe {
            let mut r = cc.read_volatile();
            log::debug!("initial controller config = {:?}", r);
            r.set_enable(false);
            cc.write_volatile(r);
        }

        // wait for CSTS.RDY = 0
        log::trace!("waiting for controller to become ready to configure");
        let csts: *mut u8 = base_address.offset(REG_CSTS).as_ptr();
        busy_wait_for_ready_bit(csts, false);

        // configure admin queues
        let doorbell_base = base_address.offset(0x1000);

        let mut admin_cq = queue::CompletionQueue::new(
            0,
            (PAGE_SIZE / COMPLETION_ENTRY_SIZE) as u16,
            doorbell_base,
            cap.doorbell_stride(),
        )?;

        let mut admin_sq = queue::SubmissionQueue::new(
            0,
            (PAGE_SIZE / SUBMISSION_ENTRY_SIZE) as u16,
            doorbell_base,
            cap.doorbell_stride(),
            &mut admin_cq,
        )?;

        let mut admin_queue_attrbs = AdminQueueAttributes(0);
        // make each queue exactly 1 page worth of entries
        admin_queue_attrbs.set_submission_queue_size(admin_cq.size());
        admin_queue_attrbs.set_completion_queue_size(admin_sq.size());
        unsafe {
            base_address
                .offset(REG_AQA)
                .as_ptr::<AdminQueueAttributes>()
                .write_volatile(admin_queue_attrbs);
        }
        unsafe {
            base_address
                .offset(REG_ASQ)
                .as_ptr::<u64>()
                .write_volatile(admin_sq.address().0 as u64);
            base_address
                .offset(REG_ACQ)
                .as_ptr::<u64>()
                .write_volatile(admin_cq.address().0 as u64);
        }

        // configure controller
        unsafe {
            let mut r = ControllerConfigReg(0);
            r.set_arbitration_mechanism(0b000); // round robin, no weights
            r.set_io_command_set_selected(0b000);
            r.set_memory_page_size(PAGE_SIZE.ilog2() - 12);
            // TODO: where should these actually come from? the spec says that you can read the
            // required and maximum values from the Identify result, but we haven't sent that yet
            r.set_io_submission_queue_entry_size(SUBMISSION_ENTRY_SIZE.ilog2());
            r.set_io_completion_queue_entry_size(COMPLETION_ENTRY_SIZE.ilog2());

            // set CC.EN = 1 to enable controller
            r.set_enable(true);
            cc.write_volatile(r);
        }

        // wait for CSTS.RDY = 1
        log::trace!("waiting for controller to become ready after enabling");
        busy_wait_for_ready_bit(csts, true);

        Ok(PcieDriver {})
    }
}

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

    let base_address = hdr.base_address(0);

    log::debug!(
        "NVMe base address = {:x} ({:x} <? {:x})",
        base_address,
        base_address as usize + base.mmio.0,
        base.mmio_size
    );

    let base_vaddress = pcie::PCI_MMIO_START.offset((base_address as usize - base.mmio.0) as isize);
    log::debug!("vbar = {}", base_vaddress);

    let nvme_version = unsafe { base_vaddress.offset(REG_VS).as_ptr::<u32>().read_volatile() };

    log::info!("device supports NVMe version {nvme_version:x}");

    let mut msix_table = None;

    for cap in hdr.capabilities() {
        match cap {
            pcie::CapabilityBlock::MsiX(msix) => {
                log::debug!("NVMe device uses MSI-X: {msix:?}");
                msix_table = Some(pcie::msix::MsiXTable::from_config(hdr, &msix));
                msix.enable();
                break;
            }
            _ => {}
        }
    }

    // TODO: error handling
    Ok(Box::new(PcieDriver::configure(base_vaddress).unwrap()))
}
