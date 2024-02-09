//! User-space, with scheduled [threads][Thread] grouped into [processes][Process].
//! Contains the thread scheduler and system message dispatch infrastructure.
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

/// The unique ID of a process.
// TODO: make type NonZeroU32 instead
pub type ProcessId = u32;

/// A user-space process.
///
/// A process is a collection of threads that share the same address space and system resources.
pub struct Process {
    /// The ID of this process.
    pub id: ProcessId,
    /// The IDs of the threads running in this process.
    pub threads: SmallVec<[ThreadId; 4]>,
    page_tables: PageTable,
    mapped_files: HashMap<VirtualAddress, MappedFile>,
    channel: Channel,
    address_space_allocator: VirtualAddressAllocator,
}

impl Process {
    /// Get the address space ID (ASID) for this process's page tables.
    pub fn asid(&self) -> u16 {
        self.page_tables.asid
    }

    /// Process any new commands that have been recieved on this process's message channel.
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

    /// Handle a page fault caused by a thread in this process.
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

/// The global table of processes by ID.
pub fn processes() -> &'static CHashMapG<ProcessId, Process> {
    unsafe { PROCESSES.get_or_init(Default::default) }
}

/// Cause a process to exit by ID.
// TODO: this should be somewhere else, perhaps with `spawn`.
fn exit_process(pid: ProcessId) {
    log::trace!("process {pid} exited");
    let mut proc = processes()
        .remove(&pid)
        .expect("processes only exit once and can only exit if they have already started running");

    // delete and unschedule all threads
    for tid in proc.threads.into_iter() {
        scheduler().remove_thread(tid);
        threads().remove(&tid).expect("process only has valid tids");
        // drop thread
    }

    // deal with async resources, and then drop the process (by moving it into the task)
    crate::tasks::spawn(async move {
        // free physical memory mapped in process page table, flushing resources as we go
        for mapped_region in proc.page_tables.iter() {
            if let Some((map_base_addr, res)) = proc.mapped_files.iter_mut().find(|(ba, r)| {
                interface::resource::resource_maps(ba, r, mapped_region.base_virt_addr)
            }) {
                // TODO: we need to check if the page is dirty
                match res
                    .resource
                    .flush_pages(
                        (mapped_region.base_virt_addr.0 - map_base_addr.0) as u64,
                        mapped_region.base_phys_addr,
                        mapped_region.page_count,
                    )
                    .await
                {
                    Ok(()) => {}
                    Err(e) => {
                        log::error!("failed to flush mapped resource @ {map_base_addr}/{} in exiting process {pid}: {e}", mapped_region.base_virt_addr);
                    }
                }
            }

            physical_memory_allocator()
                .free_pages(mapped_region.base_phys_addr, mapped_region.page_count);
        }

        // drop process, releasing its resources
        // TODO: make sure we don't double free the channel
    });

    // removing this processes' threads will schedule a new thread from a different process to run next
}

/// Register system calls related to processes and thread scheduling.
pub fn register_system_call_handlers() {
    use kapi::system_calls::SystemCallNumber;
    let h = crate::exception::system_call_handlers();
    h.insert(SystemCallNumber::Exit as u16, |_id, pid, _tid, _regs| {
        exit_process(pid);
    });
    h.insert(SystemCallNumber::Yield as u16, |_id, _pid, _tid, _regs| {
        scheduler().schedule_next_thread();
    });
    h.insert(SystemCallNumber::WaitForMessage as u16, |_id, _pid, tid, _regs| {
        threads().get_mut(&tid)
            .expect("valid thread ID is currently running")
            .state = ThreadState::Waiting;
        // TODO: where do we check for messages to resume execution?
        todo!("which thread will resume when the process recieves a message which could equally be for any thread?");
        // TODO: perhaps each thread should have its own channel, or we should otherwise do something about that?
        scheduler().schedule_next_thread();
    });
}
