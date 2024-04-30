//! Initialization routines that are called during the boot process by `kmain` to setup the system.
use self::platform::uart::DebugUartLogger;

use super::*;
use crate::{
    ds::dtb::DeviceTree,
    memory::{VirtualAddress, PAGE_SIZE},
    platform::{timer, CpuId},
    registry::Path,
};
use alloc::sync::Arc;
use byteorder::BigEndian;
use hashbrown::HashMap;
use spin::once::Once;

static LOGGER: Once<DebugUartLogger> = Once::new();

/// Configure logging using [log] and the [platform::uart::DebugUartLogger].
pub fn logging(log_level: log::LevelFilter) {
    log::set_logger(LOGGER.call_once(DebugUartLogger::default)).expect("set logger");
    log::set_max_level(log_level);
    log::info!("starting kernel!");
}

fn default_init_process_path() -> &'static str {
    "/volumes/root/bin/init"
}

/// Kernel "command line" parameters.
///
/// The kernel deserializes this struct from JSON as found in the bootargs device tree parameter.
/// Use `env set bootargs ...` to set these in u-boot before booting the kernel.
#[derive(serde::Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
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
pub fn configure_time_slicing() {
    let props = timer::properties();
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

/// Bring up the rest of the CPUs in the system (if present).
pub fn smp_start(dt: &DeviceTree) {
    log::info!("starting up remaining CPU cores for SMP");
    let psci = platform::psci::Psci::new(dt);
    log::debug!("PSCI implementation: {psci:x?}");

    let entry_point_address = VirtualAddress(_secondary_start as usize).to_physical_canonical();
    let boot_cpu_id = intrinsics::current_cpu_id();

    // Called for each discovered CPU in the device tree to enable that CPU.
    let power_on_cpu = move |cpu_block_name: &str, cpu_id: CpuId, cpu_enable_method: &[u8]| {
        log::trace!(
            "powering on CPU {cpu_block_name} (id: {cpu_id}, enable method: {})",
            core::str::from_utf8(cpu_enable_method).unwrap_or("?")
        );
        match cpu_enable_method {
                b"psci\0" if psci.is_some() => {
                    // use a 4MiB stack, just like the first core (see link.ld).
                    let stack = memory::PhysicalBuffer::alloc(4 * 1024 * 1024 / PAGE_SIZE, &Default::default()).unwrap();
                    match psci.as_ref().unwrap().cpu_on(cpu_id, entry_point_address, stack.virtual_address().0 + stack.len()) {
                        Ok(()) => {
                            log::trace!("successfully powered on CPU #{cpu_id}");
                            // TODO: ostensibly if we turn off the CPU we need to delete the stack, but otherwise it lives forever
                            core::mem::forget(stack);
                        },
                        Err(e) => { log::error!("failed to power on CPU #{cpu_id} ({cpu_block_name}): {e}"); },
                    }
                },
                _ => log::warn!("CPU {cpu_block_name} (id = {cpu_id}) has unknown or unsupported enable method: {:?}", ds::dtb::StringList::new(cpu_enable_method))
            }
    };

    // Iterate over the device tree and find each CPU node, then identify the CPU ID and enable method properties.
    // Once the node has been processed, the CPU is powered on using `power_on_cpu(...)`.
    let dti = dt
        .iter_structure()
        .skip_while(|i| !matches!(i, ds::dtb::StructureItem::StartNode("cpus")));
    let mut cpu_block_name = None;
    let mut cpu_id = None;
    let mut cpu_enable_method: Option<&[u8]> = None;
    for i in dti {
        match i {
            ds::dtb::StructureItem::StartNode(s) => {
                if s.starts_with("cpu@") {
                    cpu_block_name = Some(s);
                }
            }
            ds::dtb::StructureItem::EndNode => {
                if let Some(cpu_block_name) = cpu_block_name.take() {
                    match (cpu_id.take(), cpu_enable_method.take()) {
                        (Some(cpu_id), Some(cpu_enable_method)) => {
                            if cpu_id != boot_cpu_id {
                                power_on_cpu(cpu_block_name, cpu_id, cpu_enable_method)
                            }
                        }
                        _ => {
                            log::warn!("device tree contained incomplete CPU block {cpu_block_name}: CPU ID = {cpu_id:?}, CPU enable method = {cpu_enable_method:?}");
                        }
                    }
                }
            }
            ds::dtb::StructureItem::Property { name, data, .. } if cpu_block_name.is_some() => {
                match name {
                    "reg" => {
                        cpu_id = Some(BigEndian::read_u32(data) as usize);
                    }
                    "enable-method" => {
                        cpu_enable_method = Some(data);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    log::debug!("SMP startup finished!");
}

/// Spawn the task executor thread as a kernel thread.
pub fn spawn_task_executor_thread() {
    log::info!("creating task executor thread");
    let task_stack = memory::PhysicalBuffer::alloc(4 * 1024, &Default::default())
        .expect("allocate task exec thread stack");
    log::debug!("task stack = {task_stack:x?}");

    process::thread::spawn_thread(Arc::new(process::Thread::kernel_thread(
        process::thread::TASK_THREAD,
        tasks::run_executor,
        task_stack,
    )));
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
    fs::fat::mount(Path::new("/volumes/root"), bs)
        .await
        .unwrap();

    log::info!("spawning init process");
    let init_proc = process::spawn_process(opts.init_process_path, None, |_| ())
        .await
        .expect("spawn init process");

    log::info!("init pid = {}", init_proc.id);

    let main_thread = {
        init_proc
            .threads
            .lock()
            .await
            .first()
            .expect("init process has main thread")
            .clone()
    };

    let ec = main_thread.exit_code().await;
    log::warn!("init process exited with code {ec:?}");
    qemu_exit::AArch64::new().exit(match ec {
        kapi::completions::ThreadExit::Normal(c) => c.into(),
        kapi::completions::ThreadExit::PageFault => 0x1_0000,
        kapi::completions::ThreadExit::Killed => 0x1_0001,
    });
}
