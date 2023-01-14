use crate::{exception::MsiDescriptor, memory::PhysicalAddress};

use super::MsiController;

pub struct ItsMsiController {}

impl ItsMsiController {
    pub fn init(base: PhysicalAddress, reg_size: usize) -> ItsMsiController {
        log::info!(
            "initializing ITS MSI controller @ {}, size 0x{:x}",
            base,
            reg_size
        );
        todo!();
    }
}

impl MsiController for ItsMsiController {
    fn alloc_msi(&mut self) -> MsiDescriptor {
        todo!()
    }
}
