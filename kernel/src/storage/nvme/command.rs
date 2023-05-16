use crate::io::LogicalAddress;

use super::queue::{Command, QueueId};

#[repr(u8)]
pub enum IdentifyStructure {
    Namespace = 0x00,
    Controller = 0x01,
    ActiveNamespaceList = 0x02,
}

#[repr(u8)]
pub enum QueuePriority {
    Urgent = 0b00,
    High = 0b01,
    Medium = 0b10,
    Low = 0b11,
}

impl<'sq> Command<'sq> {
    // Admin commands //
    pub fn identify(self, controller_id: u16, structure_id: IdentifyStructure) -> Self {
        self.set_opcode(0x06)
            .set_dword(10, (controller_id as u32) << 16 | structure_id as u32)
    }

    pub fn create_io_completion_queue(
        self,
        queue_id: QueueId,
        queue_size: u16,
        interrupt_vector_index: u16,
        interrupts_enabled: bool,
        physically_continuous: bool,
    ) -> Self {
        self.set_opcode(0x05)
            .set_dword(10, ((queue_size as u32) << 16) | (queue_id as u32))
            .set_dword(
                11,
                ((interrupt_vector_index as u32) << 16)
                    | (if interrupts_enabled { 2 } else { 0 })
                    | (if physically_continuous { 1 } else { 0 }),
            )
    }

    pub fn delete_io_completion_queue(self, queue_id: QueueId) -> Self {
        self.set_opcode(0x04).set_dword(10, queue_id as u32)
    }

    pub fn create_io_submission_queue(
        self,
        queue_id: QueueId,
        queue_size: u16,
        completion_queue_id: QueueId,
        priority: QueuePriority,
        physically_continuous: bool,
    ) -> Self {
        self.set_opcode(0x01)
            .set_dword(10, ((queue_size as u32) << 16) | (queue_id as u32))
            .set_dword(
                11,
                ((completion_queue_id as u32) << 16)
                    | ((priority as u32) << 1)
                    | (if physically_continuous { 1 } else { 0 }),
            )
            // TODO: this is supposed to be the NVM set ID, but is optional
            .set_dword(12, 0)
    }

    pub fn delete_io_submission_queue(self, queue_id: QueueId) -> Self {
        self.set_opcode(0x00).set_dword(10, queue_id as u32)
    }

    // IO commands //
    pub fn flush(self) -> Self {
        self.set_opcode(0x00)
    }

    pub fn read(self, starting_lba: LogicalAddress, num_blocks: u16) -> Self {
        self.set_opcode(0x02)
            .set_qword(10, starting_lba.0)
            // TODO: not all parameters are exposed, including data protection
            // parameters
            .set_dword(12, num_blocks as u32)
            // TODO: lots of hints here, not exposed
            .set_dword(13, 0)
        // TODO: 14.. have end-to-end values that are not yet relevant
    }

    pub fn write(self, starting_lba: LogicalAddress, num_blocks: u16) -> Self {
        self.set_opcode(0x01)
            .set_qword(10, starting_lba.0)
            // TODO: not all parameters are exposed, including data protection
            // parameters and "directives"
            .set_dword(12, num_blocks as u32)
            // TODO: lots of hints here, not exposed
            .set_dword(13, 0)
        // TODO: 14.. have end-to-end values that are not yet relevant
    }
}
