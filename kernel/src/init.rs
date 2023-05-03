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
