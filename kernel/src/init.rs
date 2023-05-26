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
    let props = timer::find_timer_properties(&dt);
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
            log::trace!("{id} timer interrupt! {}", timer::counter());
            process::scheduler::scheduler().schedule_next_thread();
            timer::write_timer_value(timer::frequency() >> 4);
        }),
    );

    // set timer to go off after we halt
    timer::write_timer_value(timer::frequency() >> 4);
}
