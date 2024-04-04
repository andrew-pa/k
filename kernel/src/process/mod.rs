//! User-space, with scheduled [threads][Thread] grouped into [processes][Process].
//! Contains the thread scheduler and system message dispatch infrastructure.
use crate::{
    ds::lists::ConcurrentLinkedList,
    exception::Registers,
    memory::{
        paging::{PageTable, PageTableEntryOptions},
        physical_memory_allocator, PhysicalBuffer, VirtualAddress, VirtualAddressAllocator,
        PAGE_SIZE,
    },
    tasks::locks::Mutex,
};
use alloc::sync::Arc;

use bitfield::Bit;
use core::{
    num::NonZeroU32,
    sync::atomic::{AtomicU16, AtomicU32},
};
use kapi::{commands::Command, completions::Completion};
use smallvec::SmallVec;
use snafu::{OptionExt, ResultExt};
use spin::Once;

pub mod thread;
pub use thread::{scheduler::scheduler, threads, Thread, ThreadId};

pub mod interface;

mod spawn;
pub use spawn::spawn_process;

use thread::*;

use interface::OwnedQueue;

pub use kapi::ProcessId;

/// A user-space process.
///
/// A process is a collection of threads that share the same address space and system resources.
/// For a process to end, *all* of its associated threads must first exit.
pub struct Process {
    /// The ID of this process.
    pub id: ProcessId,
    /// The threads running in this process.
    pub threads: Mutex<SmallVec<[Arc<Thread>; 2]>>,
    /// Page tables for this process.
    page_tables: PageTable,
    /// Allocator for the virtual address space of the process.
    address_space_allocator: Mutex<VirtualAddressAllocator>,
    /// The next free queue ID.
    #[allow(dead_code)]
    next_queue_id: AtomicU16,
    /// The send (user space to kernel) queues associated with this process, and their associated
    /// receive queue.
    #[allow(clippy::type_complexity)]
    send_queues: ConcurrentLinkedList<(Arc<OwnedQueue<Command>>, Arc<OwnedQueue<Completion>>)>,
    /// The receive (kernel to user space) queues associated with this process.
    // TODO: these queues don't need to be owned, because the process implicitly owns
    // all the mapped memory in its page tables by default.
    recv_queues: ConcurrentLinkedList<Arc<OwnedQueue<Completion>>>,
}

crate::assert_sync!(Process);

impl Process {
    /// Get the address space ID (ASID) for this process's page tables.
    pub fn asid(&self) -> u16 {
        self.page_tables.asid
    }

    /// Process any new commands that have been recieved on this process's message channel.
    pub fn dispatch_new_commands(self: &Arc<Process>) {
        // downgrade the Arc so that pending tasks don't keep the process alive unnecessarily
        let this = &Arc::downgrade(self);
        for (send_qu, assoc_recv_qu) in self.send_queues.iter() {
            let assoc_recv_qu = Arc::downgrade(assoc_recv_qu);
            while let Some(cmd) = send_qu.poll() {
                let this = this.clone();
                let assoc_recv_qu = assoc_recv_qu.clone();
                log::trace!("[pid {}, SQ {}]: {cmd:?}", self.id, send_qu.id);
                crate::tasks::spawn(async move {
                    if let Some(proc) = this.upgrade() {
                        let cmpl = proc.dispatch_command(cmd, assoc_recv_qu.clone()).await;
                        if let Some(arq) = assoc_recv_qu.upgrade() {
                            log::trace!("[pid {}, RQ {}]: {cmpl:?}", proc.id, arq.id);
                            match arq.post(cmpl) {
                                Ok(()) => {}
                                Err(_) => {
                                    todo!("kill process on queue overflow")
                                }
                            }
                        }
                    }
                });
            }
        }
    }

    /// Handle a page fault caused by a thread in this process.
    pub async fn on_page_fault(&self, thread: Arc<Thread>, address: VirtualAddress) {
        log::trace!("on_page_fault {address}");

        log::error!(
            "process {}, thread {}: unhandled page fault at {address}",
            self.id,
            thread.id
        );

        thread.exit(kapi::completions::ThreadExit::PageFault);
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        for mapped_region in self.page_tables.iter() {
            physical_memory_allocator()
                .free_pages(mapped_region.base_phys_addr, mapped_region.page_count);
        }
    }
}

static PROCESSES: Once<ConcurrentLinkedList<Arc<Process>>> = Once::new();

static NEXT_PID: AtomicU32 = AtomicU32::new(1);

/// The global table of processes by ID.
pub fn processes() -> &'static ConcurrentLinkedList<Arc<Process>> {
    PROCESSES.call_once(Default::default)
}

pub fn process_for_id(id: ProcessId) -> Option<Arc<Process>> {
    processes().iter().find(|p| p.id == id).cloned()
}

/// Register system calls related to processes and thread scheduling.
pub fn register_system_call_handlers() {
    use kapi::system_calls::SystemCallNumber;
    let h = crate::exception::system_call_handlers();
    h.insert_blocking(SystemCallNumber::Exit as u16, |_id, _proc, thread, regs| {
        thread.exit(kapi::completions::ThreadExit::Normal(regs.x[0] as u16));
    });
    h.insert_blocking(
        SystemCallNumber::Yield as u16,
        |_id, _proc, _thread, _regs| {
            scheduler().schedule_next_thread();
        },
    );
    h.insert_blocking(SystemCallNumber::WaitForMessage as u16, |_id, _proc, thread, _regs| {
        thread.set_state(ThreadState::Waiting);
        // TODO: where do we check for messages to resume execution?
        todo!("which thread will resume when the process recieves a message which could equally be for any thread?");
        // TODO: perhaps each thread should have its own channel, or we should otherwise do something about that?
        // scheduler().schedule_next_thread();
    });
}
