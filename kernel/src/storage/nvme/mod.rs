use crate::{
    bus::pcie::{self, msix::MsiXTable},
    memory::{PhysicalBuffer, VirtualAddress, PAGE_SIZE},
};
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use bitfield::{bitfield, Bit};
use smallvec::SmallVec;
use spin::Mutex;

use self::queue::{CompletionQueue, SubmissionQueue};

mod command;
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
    timeout, _: 31, 24;
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

pub struct PcieDriver {
    admin_cq: Arc<Mutex<CompletionQueue>>,
    admin_sq: Arc<Mutex<SubmissionQueue>>,
}

impl pcie::DeviceDriver for PcieDriver {}

fn busy_wait_for_ready_bit(csts: *mut u8, value: bool) {
    unsafe {
        loop {
            let csts_v = csts.read_volatile();
            if csts_v.bit(0) == value {
                break;
            } else if csts_v.bit(1) {
                panic!("NVMe fatal error");
            }
        }
    }
}

impl PcieDriver {
    fn configure(
        pcie_addr: pcie::DeviceId,
        base_address: VirtualAddress,
        msix_table: MsiXTable,
    ) -> Result<PcieDriver, crate::memory::MemoryError> {
        let cap: ControllerCapabilities = unsafe {
            base_address
                .offset(REG_CAP)
                .as_ptr::<ControllerCapabilities>()
                .read_volatile()
        };
        log::trace!("CAP = {:?}", cap);

        let csts: *mut u8 = base_address.offset(REG_CSTS).as_ptr();
        log::debug!("checking to see if the controller was ever ready");
        busy_wait_for_ready_bit(csts, true);

        // reset the controller
        let cc: *mut ControllerConfigReg = base_address.offset(REG_CC).as_ptr();
        unsafe {
            let mut r = cc.read_volatile();
            log::trace!("initial controller config = {:?}", r);
            r.set_enable(false);
            cc.write_volatile(r);
        }

        // see §7.6.1 for details on how to initialize the controller

        // 2. wait for CSTS.RDY = 0
        log::debug!("waiting for controller to become ready to configure");
        busy_wait_for_ready_bit(csts, false);

        // 3. configure admin queues
        let doorbell_base = base_address.offset(0x1000);

        // make each queue exactly 1 page worth of entries
        let mut admin_cq = CompletionQueue::new_admin(
            (PAGE_SIZE / COMPLETION_ENTRY_SIZE) as u16,
            doorbell_base,
            cap.doorbell_stride(),
        )?;

        let mut admin_sq = SubmissionQueue::new_admin(
            (PAGE_SIZE / SUBMISSION_ENTRY_SIZE) as u16,
            doorbell_base,
            cap.doorbell_stride(),
            &mut admin_cq,
        )?;

        unsafe {
            log::trace!(
                "old admin queue addresses: rx@{:x}, tx@{:x}; new attribs={:?}",
                base_address.offset(REG_ACQ).as_ptr::<u64>().read_volatile(),
                base_address.offset(REG_ASQ).as_ptr::<u64>().read_volatile(),
                base_address
                    .offset(REG_AQA)
                    .as_ptr::<AdminQueueAttributes>()
                    .read_volatile()
            );
        }

        let mut admin_queue_attrbs = AdminQueueAttributes(0);
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
        log::trace!(
            "new admin queue addresses: rx@{}, tx@{}; new attribs={:?}",
            admin_cq.address(),
            admin_sq.address(),
            admin_queue_attrbs
        );

        // 4. configure controller
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
            log::trace!("new controller config = {:?}", r);
            cc.write_volatile(r);
        }

        // 6. wait for CSTS.RDY = 1
        log::debug!("waiting for controller to become ready after enabling");
        // tell the MMU that this region is volatile and shouldn't be cached
        busy_wait_for_ready_bit(csts, true);
        log::debug!("controller ready!");

        // 7. send Identify commands
        // the identify structure returns a 4KiB structure which is fortunatly only a single
        // page
        let id_res_buf = PhysicalBuffer::alloc(1, &Default::default())?;

        log::trace!("sending identify command to controller");
        admin_sq
            .begin()
            .expect("queue just created, can not be full")
            .set_command_id(0xa000)
            .identify(0, command::IdentifyStructure::Controller)
            .set_data_ptr_single(id_res_buf.physical_address())
            .submit();

        log::trace!("waiting for identify command completion");
        let c = admin_cq.busy_wait_for_completion();
        log::debug!("ID ctrl completion: {c:?}");
        let id_res: *mut u16 = id_res_buf.virtual_address().as_ptr();
        unsafe {
            log::info!(
                "nvme controller PCIe vendor id = {:x}, sqes/cqes={:x}",
                id_res.read_volatile(),
                id_res.offset(512 / 2).read_volatile()
            );
        }

        log::trace!("requesting active namespace list");
        admin_sq
            .begin()
            .expect("queue just created, can not be full")
            .set_command_id(0xa001)
            .identify(0, command::IdentifyStructure::ActiveNamespaceList)
            .set_namespace_id(0)
            .set_data_ptr_single(id_res_buf.physical_address())
            .submit();
        log::trace!("waiting for command completion");
        let c = admin_cq.busy_wait_for_completion();
        log::debug!("ID active namespace list completion: {c:?}");
        let id_res: *mut u32 = id_res_buf.virtual_address().as_ptr();
        let mut namespace_ids = Vec::new();
        unsafe {
            for i in 0..1024 {
                let val = id_res.offset(i).read();
                if val == 0 {
                    break;
                }
                namespace_ids.push(val);
            }
        }
        log::debug!("active namespaces: {namespace_ids:?}");

        let admin_sq = Arc::new(Mutex::new(admin_sq));

        // create IO queues
        // allocate MSI for IO completion queue
        let msi = {
            let mut ic = crate::exception::interrupt_controller();
            ic.alloc_msi().expect("MSI support for NVMe")
        };
        let ivx = 1u16;
        msix_table.write(ivx as usize, &msi);
        log::trace!("creating IO completion queue");
        let mut io_cq = CompletionQueue::new_io(
            1,
            (2 * PAGE_SIZE / COMPLETION_ENTRY_SIZE) as u16,
            doorbell_base,
            cap.doorbell_stride(),
            admin_sq.clone(),
            &mut admin_cq,
            ivx,
            true,
        )
        .expect("create IO completion queue");

        log::trace!("creating IO submission queue");
        let mut io_sq = SubmissionQueue::new_io(
            1,
            (2 * PAGE_SIZE / SUBMISSION_ENTRY_SIZE) as u16,
            doorbell_base,
            cap.doorbell_stride(),
            &mut io_cq,
            admin_sq.clone(),
            &mut admin_cq,
            command::QueuePriority::Medium,
        )
        .expect("create IO submission queue");

        let admin_cq = Arc::new(Mutex::new(admin_cq));

        // >>>>>>>>>>>>>>>>>>>>>>>>>>>> LEFT OFF HERE <<<<<<<<<<<<<<<<<<<<<<<<<<<<<<<<
        // step 7
        // TODO:
        // ~ send Identify command and parse results (is there anything useful in here??)
        // - set up interrupts
        // - find somewhere to keep block devices & define block device interface
        // - impl block device interface for NVMe

        log::info!("NVMe device at {pcie_addr} initialized!");

        Ok(PcieDriver { admin_cq, admin_sq })
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
    Ok(Box::new(
        PcieDriver::configure(
            addr,
            base_vaddress,
            msix_table.expect("MSI-X is only current supported MSI scheme for NVMe driver"),
        )
        .unwrap(),
    ))
}
