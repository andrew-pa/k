//! Initialization routines that are called during the boot process by `kmain` to setup the system.
use super::*;
use crate::{ds::dtb::DeviceTree, registry::Path};
use hashbrown::HashMap;

/// Configure logging using [log] and the [uart::DebugUartLogger].
pub fn logging(log_level: log::LevelFilter) {
    log::set_logger(&uart::DebugUartLogger).expect("set logger");
    log::set_max_level(log_level);
    log::info!("starting kernel!");
}

fn default_init_process_path() -> &'static str {
    "/fat/init"
}

/// Kernel "command line" parameters.
///
/// The kernel deserializes this struct from JSON as found in the bootargs device tree parameter.
/// Use `env set bootargs ...` to set these in u-boot before booting the kernel.
#[derive(serde::Deserialize, Debug, Default)]
pub struct BootOptions<'a> {
    #[serde(default = "default_init_process_path")]
    init_process_path: &'a str,
}

/// Find and parse the boot options from the device tree `/chosen/bootargs` field if present,
/// otherwise returning the defaults.
pub fn find_boot_options<'a>(dt: &'a DeviceTree) -> BootOptions<'a> {
    let mut opts = None;
    dt.process_properties_for_node("chosen", |prop, data, _| {
        if prop == "bootargs" {
            let s = core::ffi::CStr::from_bytes_until_nul(data)
                .expect("devicetree /chosen/bootargs terminated correctly")
                .to_str()
                .expect("devicetree /chosen/bootargs valid UTF-8 string");
            log::trace!("bootargs = \"{s}\"");
            if s.trim().is_empty() {
                return;
            }
            opts = match serde_json_core::from_str(s) {
                Ok((o, _)) => Some(o),
                Err(e) => panic!("failed to parse boot options from JSON: {e} (source = {s})"),
            };
        }
    });
    log::debug!("kernel boot options = {opts:?}");
    opts.unwrap_or_default()
}

/// Configure time slicing interrupts and initialize the system timer.
///
/// The timer will trigger as soon as interrupts are enabled.
pub fn configure_time_slicing(dt: &DeviceTree) {
    let props = timer::find_timer_properties(dt);
    log::debug!("timer properties = {props:?}");
    let timer_irq = props.interrupt;

    {
        let ic = exception::interrupt_controller();
        ic.set_target_cpu(timer_irq, 0x1);
        ic.set_priority(timer_irq, 0);
        ic.set_config(timer_irq, exception::InterruptConfig::Level);
        ic.clear_pending(timer_irq);
        ic.set_enable(timer_irq, true);
    }

    timer::set_enabled(true);
    timer::set_interrupts_enabled(true);

    exception::interrupt_handlers().insert_blocking(
        timer_irq,
        Box::new(|_id, _regs| {
            //log::trace!("{id} timer interrupt! {}", timer::counter());
            process::scheduler().schedule_next_thread();
            timer::write_timer_value(timer::frequency() >> 5);
        }),
    );

    // set timer to go off as soon as we enable interrupts
    timer::write_timer_value(0);
}

/// Register system call handlers throughout the kernel in the global table.
pub fn register_system_call_handlers() {
    use kapi::system_calls::SystemCallNumber;

    // TODO: ideally this is a whole system with a ring buffer, listening, etc and also records
    // which process made the log, but for now this will do.
    exception::system_call_handlers().insert_blocking(
        SystemCallNumber::WriteLog as u16,
        |_id, _pid, _tid, regs| unsafe {
            let record = &*(regs.x[0] as *const log::Record);
            log::logger().log(record);
        },
    );

    process::register_system_call_handlers();
}

/// Initialize the PCIe bus and any devices found there.
pub fn pcie(dt: &DeviceTree) {
    let mut pcie_drivers = HashMap::new();
    pcie_drivers.insert(
        0x01080200, // MassStorage:NVM:NVMe I/O controller
        storage::nvme::init_nvme_over_pcie as bus::pcie::DriverInitFn,
    );
    bus::pcie::init(dt, &pcie_drivers);
}

/// Spawn the task executor thread as a kernel thread.
pub fn spawn_task_executor_thread() {
    log::info!("creating task executor thread");
    let task_stack = memory::PhysicalBuffer::alloc(4 * 1024, &Default::default())
        .expect("allocate task exec thread stack");
    log::debug!("task stack = {task_stack:x?}");

    unsafe {
        process::thread::spawn_thread(process::Thread::kernel_thread(
            process::thread::TASK_THREAD,
            tasks::run_executor,
            &task_stack,
        ));
    }

    // the task executor thread should continue to run while the kernel is running so we prevent
    // the stack's memory from being freed by forgetting it
    // TODO: if we need to resize the stack we need to keep track of this?
    core::mem::forget(task_stack);
}

/// This finishes the boot process, executing the asynchronous tasks required to mount the root filesystem and spawn the `init` process.
pub async fn finish_boot(opts: BootOptions<'_>) {
    log::info!("open /dev/nvme/pci@0:2:0/1");
    let bs = {
        registry::registry()
            .open_block_store(Path::new("/dev/nvme/pci@0:2:0/1"))
            .await
            .unwrap()
    };
    log::info!("mount FAT filesystem");
    fs::fat::mount(Path::new("/fat"), bs).await.unwrap();

    log::info!("spawning init process");
    let init_proc = process::spawn_process(opts.init_process_path, None::<fn(_)>)
        .await
        .expect("spawn init process");

    log::info!("init pid = {}", init_proc.id);
}
