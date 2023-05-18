use crate::{
    exception::{InterruptId, MsiDescriptor},
    memory::PhysicalAddress,
};

use super::MsiController;

pub struct V2mMsiController {
    register_addr: PhysicalAddress,
    spi_start: u32,
    num_spis: u32,
    next_spi: u32,
}

bitfield::bitfield! {
    struct V2mMsiTypeRegister(u32);
    impl Debug;
    // qemu adds 32 to this value
    spi_start, set_spi_start: 25, 16;
    num_spis, _: 9, 0;
}

impl V2mMsiController {
    pub fn init(base: PhysicalAddress, reg_size: usize) -> V2mMsiController {
        log::info!(
            "initializing v2m MSI controller @ {}, size 0x{:x}",
            base,
            reg_size
        );
        // TODO: SAFETY: assume that this is in low memory that has already been mapped
        let basep: *mut u32 = unsafe { base.to_virtual_canonical().as_ptr() };
        let iidr = unsafe { basep.offset(V2M_MSI_IIDR >> 2).read_volatile() };
        let mut typer =
            V2mMsiTypeRegister(unsafe { basep.offset(V2M_MSI_TYPER >> 2).read_volatile() });
        // TODO: why is this value not what it says in the QEMU headers?
        // perhaps there is a fixed offset for SPIs
        log::info!("V2m IIDR = 0x{iidr:x}, TYPER={typer:?}");
        // typer.set_spi_start(0x24);
        Self {
            register_addr: PhysicalAddress(base.0.wrapping_add_signed(V2M_MSI_SETSPI_NS)),
            spi_start: typer.spi_start(),
            num_spis: typer.num_spis(),
            next_spi: typer.spi_start(),
        }
    }
}

impl MsiController for V2mMsiController {
    fn alloc_msi(&mut self) -> MsiDescriptor {
        if self.next_spi > self.spi_start + self.num_spis {
            panic!("ran out of MSIs");
        }
        let intid = self.next_spi;
        log::debug!(
            "{intid} qemu spi = {}-{}",
            (intid & 0x3ff),
            (self.spi_start + 32)
        );
        self.next_spi += 1;
        MsiDescriptor {
            register_addr: self.register_addr,
            data_value: intid,
            intid,
        }
    }
}

/*
 * These definitions and the rest of this comment come from the Linux kernel source, which is the
 * only good source of information I could find about GICv2m.
 *
 * MSI_TYPER:
 *     [31:26] Reserved
 *     [25:16] lowest SPI assigned to MSI
 *     [15:10] Reserved
 *     [9:0]   Number of SPIs assigned to MSI
 */
const V2M_MSI_TYPER: isize = 0x008;
const V2M_MSI_SETSPI_NS: isize = 0x040;
const V2M_MSI_IIDR: isize = 0xFCC;

// Linux uses these to validate what comes out of the TYPER register
// const V2M_MIN_SPI: isize = 32;
// const V2M_MAX_SPI: isize = 1019;
