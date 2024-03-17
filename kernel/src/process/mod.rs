//! User-space, with scheduled [threads][Thread] grouped into [processes][Process].
//! Contains the thread scheduler and system message dispatch infrastructure.
use crate::{
    exception::Registers,
    lists::ConcurrentLinkedList,
    memory::{
        paging::{PageTable, PageTableEntryOptions},
        physical_memory_allocator, PhysicalBuffer, VirtualAddress, VirtualAddressAllocator,
        PAGE_SIZE,
    },
    tasks::locks::Mutex,
};
use alloc::sync::Arc;

use bitfield::{bitfield, Bit};
use core::{cell::OnceCell, num::NonZeroU32, sync::atomic::AtomicU32};
use smallvec::SmallVec;
use snafu::{OptionExt, ResultExt, Snafu};

pub mod thread;
pub use thread::{scheduler::scheduler, threads, Thread, ThreadId};

pub mod interface;

mod spawn;
pub use spawn::{spawn_process, SpawnError};

use interface::channel::Channel;
use thread::*;

/// The unique ID of a process.
pub type ProcessId = NonZeroU32;

/// A user-space process.
///
/// A process is a collection of threads that share the same address space and system resources.
pub struct Process {
    /// The ID of this process.
    pub id: ProcessId,
    /// The threads running in this process.
    threads: Mutex<SmallVec<[Arc<Thread>; 2]>>,
    /// Page tables for this process.
    page_tables: PageTable,
    channel: Channel,
    #[allow(dead_code)]
    address_space_allocator: VirtualAddressAllocator,
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
        while let Some(cmd) = self.channel.poll() {
            log::trace!("recieved command: {cmd:?}");
            let this = this.clone();
            crate::tasks::spawn(async move {
                if let Some(proc) = this.upgrade() {
                    let cmpl = interface::dispatch(&proc, cmd).await;
                    match proc.channel.post(&cmpl) {
                        Ok(()) => {}
                        Err(_) => {
                            todo!("kill process on queue overflow")
                        }
                    }
                }
            });
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
        // TODO: the thread needs to go into some kind of dead/error state and/or the process
        // needs to be notified that one of its threads just died. If this is the last thread
        // in the process than the process itself is now dead
    }
}

static mut PROCESSES: OnceCell<ConcurrentLinkedList<Arc<Process>>> = OnceCell::new();

static mut NEXT_PID: AtomicU32 = AtomicU32::new(1);

/// The global table of processes by ID.
pub fn processes() -> &'static ConcurrentLinkedList<Arc<Process>> {
    unsafe { PROCESSES.get_or_init(Default::default) }
}

pub fn process_for_id(id: ProcessId) -> Option<Arc<Process>> {
    processes().find(|p| p.id == id)
}

/// Cause a process to exit by ID, freeing its resources.
// TODO: this should be somewhere else, perhaps with `spawn`.
fn exit_process(proc: Arc<Process>) {
    log::trace!("process {} exited", proc.id);

    processes()
        .remove(|p| p.id == proc.id)
        .expect("processes only exit once and can only exit if they have already started running");

    // delete and unschedule all threads
    // removing the current thread will automatically schedule a new thread to run
    for thread in proc.threads.lock_blocking().drain(..) {
        scheduler().remove_thread(&thread);
        threads()
            .remove(|t| t.id == thread.id)
            .expect("process only has valid threads");
        // drop thread
    }

    // TODO: move this to drop
    // deal with async resources, and then drop the process (by moving it into the task)
    crate::tasks::spawn(async move {
        // free physical memory mapped in process page table
        for mapped_region in proc.page_tables.iter() {
            physical_memory_allocator()
                .free_pages(mapped_region.base_phys_addr, mapped_region.page_count);
        }

        // drop process, releasing its resources
        // TODO: make sure we don't double free the channel
    });
}

/// Register system calls related to processes and thread scheduling.
pub fn register_system_call_handlers() {
    use kapi::system_calls::SystemCallNumber;
    let h = crate::exception::system_call_handlers();
    h.insert_blocking(
        SystemCallNumber::Exit as u16,
        |_id, proc, _thread, _regs| {
            exit_process(proc);
        },
    );
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
