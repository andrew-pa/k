use crate::memory::PhysicalAddress;

use super::MsiController;

pub struct ItsMsiController {}

impl ItsMsiController {
    pub fn init(its_base: PhysicalAddress, its_reg_size: usize) -> ItsMsiController {
        log::info!(
            "initializing ITS MSI controller @ {}, size 0x{:x}",
            its_base,
            its_reg_size
        );
        todo!();
    }
}

impl MsiController for ItsMsiController {
    fn alloc_msi(&self) -> crate::exception::MsiDescriptor {
        todo!()
    }
}
