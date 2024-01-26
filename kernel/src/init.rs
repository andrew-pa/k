use alloc::boxed::Box;

use crate::dtb::DeviceTree;

use super::*;

pub fn init_logging(log_level: log::LevelFilter) {
    log::set_logger(&uart::DebugUartLogger).expect("set logger");
    log::set_max_level(log_level);
    log::info!("starting kernel!");

    let current_el = current_el();
    log::debug!("current EL = {current_el}");
    log::debug!("MAIR = 0x{:x}", mair());

    if current_el != 1 {
        todo!("switch from {current_el} to EL1 at boot");
    }
}

pub fn configure_time_slicing(dt: &DeviceTree) {
    let props = timer::find_timer_properties(dt);
    log::debug!("timer properties = {props:?}");
    let timer_irq = props.interrupt;

    {
        let ic = exception::interrupt_controller();
        ic.set_target_cpu(timer_irq, 0x1);
        ic.set_priority(timer_irq, 0);
        ic.set_config(timer_irq, exception::InterruptConfig::Level);
        ic.set_pending(timer_irq, false);
        ic.set_enable(timer_irq, true);
    }

    timer::set_enabled(true);
    timer::set_interrupts_enabled(true);

    exception::interrupt_handlers().insert(
        timer_irq,
        Box::new(|id, regs| {
            //log::trace!("{id} timer interrupt! {}", timer::counter());
            process::scheduler().schedule_next_thread();
            timer::write_timer_value(timer::frequency() >> 5);
        }),
    );

    // set timer to go off as soon as we enable interrupts
    timer::write_timer_value(0);
}

pub fn register_system_call_handlers() {
    use kapi::system_calls::SystemCallNumber;

    // TODO: ideally this is a whole system with a ring buffer, listening, etc and also records
    // which process made the log, but for now this will do.
    exception::system_call_handlers().insert(
        SystemCallNumber::WriteLog as u16,
        |id, pid, tid, regs| unsafe {
            let record = &*(regs.x[0] as *const log::Record);
            log::logger().log(record);
        },
    );

    process::register_system_call_handlers();
}

pub fn spawn_task_executor_thread() {
    log::info!("creating task executor thread");
    let task_stack = memory::PhysicalBuffer::alloc(4 * 1024, &Default::default())
        .expect("allocate task exec thread stack");
    log::debug!("task stack = {task_stack:x?}");

    process::thread::spawn_thread(process::Thread::kernel_thread(
        process::thread::TASK_THREAD,
        tasks::run_executor,
        &task_stack,
    ));

    // the task executor thread should continue to run while the kernel is running so we prevent
    // the stack's memory from being freed by forgetting it
    // TODO: if we need to resize the stack we need to keep track of this?
    core::mem::forget(task_stack);
}
