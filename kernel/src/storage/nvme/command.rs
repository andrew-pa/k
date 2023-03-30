use super::queue::Command;

#[repr(u8)]
pub enum IdentifyStructure {
    Namespace = 0x00,
    Controller = 0x01,
}

impl<'sq> Command<'sq> {
    // Admin commands //
    pub fn identify(self, controller_id: u16, structure_id: IdentifyStructure) -> Command<'sq> {
        self.set_opcode(0x06)
            .set_dword(10, (controller_id as u32) << 16 | structure_id as u32)
    }
}
