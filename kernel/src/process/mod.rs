use crate::{
    exception::Registers,
    memory::{
        paging::{PageTable, PageTableEntryOptions},
        physical_memory_allocator, MemoryError, PhysicalAddress, PhysicalBuffer, VirtualAddress,
        VirtualAddressAllocator, PAGE_SIZE,
    },
    CHashMapG,
};
use alloc::{boxed::Box, vec::Vec};
use async_trait::async_trait;
use bitfield::{bitfield, Bit};
use core::{cell::OnceCell, error::Error, sync::atomic::AtomicU32};
use futures::Future;
use hashbrown::HashMap;
use smallvec::{smallvec, SmallVec};
use snafu::{ensure, OptionExt, ResultExt, Snafu};

pub mod thread;
pub use thread::{scheduler::scheduler, threads, Thread, ThreadId};

pub mod interface;

mod spawn;
pub use spawn::{spawn_process, SpawnError};

use interface::{channel::Channel, resource::MappedFile};
use thread::*;

// TODO: make type NonZeroU32 instead
pub type ProcessId = u32;

pub struct Process {
    pub id: ProcessId,
    pub page_tables: PageTable,
    pub threads: SmallVec<[ThreadId; 4]>,
    pub mapped_files: HashMap<VirtualAddress, MappedFile>,
    pub channel: Channel,
    address_space_allocator: VirtualAddressAllocator,
}

impl Process {
    pub fn dispatch_new_commands(&mut self, tid: ThreadId) {
        while let Some(cmd) = self.channel.poll() {
            log::trace!("recieved command: {cmd:?}");
            let pid = self.id;
            crate::tasks::spawn(async move {
                let cmpl = interface::dispatch(pid, tid, cmd).await;
                let mut proc = processes().get_mut(&pid).unwrap();
                match proc.channel.post(&cmpl) {
                    Ok(()) => {}
                    Err(e) => {
                        todo!("kill process on queue overflow")
                    }
                }
            });
        }
    }

    pub async fn on_page_fault(&mut self, tid: ThreadId, address: VirtualAddress) {
        log::trace!("on_page_fault {address}");
        if let Some((base_address, res)) = self
            .mapped_files
            .iter_mut()
            .find(|(ba, r)| interface::resource::resource_maps(ba, r, address))
        {
            match res
                .on_page_fault(*base_address, address, &mut self.page_tables)
                .await
            {
                Ok(()) => {
                    // resume thread
                    threads().get_mut(&tid).expect("thread ID valid").state = ThreadState::Running;
                }
                Err(e) => {
                    log::error!(
                        "page fault handler failed for process {} at address {address}: {e}",
                        self.id
                    );
                    // TODO: how do we inform the process it has an error?
                }
            }
        } else {
            log::error!(
                "process {}, thread {}: unhandled page fault at {address}",
                self.id,
                tid
            );
            // TODO: the thread needs to go into some kind of dead/error state and/or the process
            // needs to be notified that one of its threads just died. If this is the last thread
            // in the process than the process itself is now dead
        }
    }

    /// Make a file available to the process via mapped memory.
    /// Physical memory will only be allocated when the process accesses the region for the first time.
    /// Returns the base address and length in bytes of the mapped region.
    pub fn attach_file(
        &mut self,
        resource: Box<dyn crate::fs::File>,
    ) -> Result<(VirtualAddress, usize), MemoryError> {
        let length_in_bytes = resource.len() as usize;
        let num_pages = length_in_bytes.div_ceil(PAGE_SIZE);
        let base_addr = self.address_space_allocator.alloc(num_pages)?;
        self.mapped_files.insert(
            base_addr,
            MappedFile {
                length_in_bytes,
                resource,
            },
        );
        Ok((base_addr, length_in_bytes))
    }
}

// TODO: what we really want here is a concurrent SlotMap
static mut PROCESSES: OnceCell<CHashMapG<ProcessId, Process>> = OnceCell::new();

static mut NEXT_PID: AtomicU32 = AtomicU32::new(1);

pub fn processes() -> &'static CHashMapG<ProcessId, Process> {
    unsafe { PROCESSES.get_or_init(Default::default) }
}
