use crate::memory::PhysicalAddress;

use super::MsiController;

pub struct V2mMsiController {}

impl V2mMsiController {
    pub fn init(its_base: PhysicalAddress, its_reg_size: usize) -> V2mMsiController {
        log::info!(
            "initializing v2m MSI controller @ {}, size 0x{:x}",
            its_base,
            its_reg_size
        );
        todo!();
    }
}

impl MsiController for V2mMsiController {
    fn alloc_msi(&self) -> crate::exception::MsiDescriptor {
        todo!()
    }
}
