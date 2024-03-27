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
use alloc::{sync::Arc, vec::Vec};

use bitfield::Bit;
use core::{
    num::NonZeroU32,
    sync::atomic::{AtomicU16, AtomicU32},
    task::Waker,
};
use futures::Future;
use kapi::{commands::Command, completions::Completion};
use smallvec::SmallVec;
use snafu::{OptionExt, ResultExt};
use spin::{Mutex as SpinMutex, Once};

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
pub struct Process {
    /// The ID of this process.
    pub id: ProcessId,
    /// The threads running in this process.
    threads: Mutex<SmallVec<[Arc<Thread>; 2]>>,
    /// Page tables for this process.
    page_tables: PageTable,
    /// Allocator for the virtual address space of the process.
    #[allow(dead_code)]
    address_space_allocator: VirtualAddressAllocator,
    /// The next free queue ID.
    #[allow(dead_code)]
    next_queue_id: AtomicU16,
    /// The queues associated with this process.
    #[allow(clippy::type_complexity)]
    queues: ConcurrentLinkedList<(Arc<OwnedQueue<Command>>, Arc<OwnedQueue<Completion>>)>,
    /// The exit code this process exited with (if it has exited).
    exit_code: Once<Option<u32>>,
    /// Future wakers for futures waiting on exit code.
    exit_waker: SpinMutex<Vec<Waker>>,
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
        for (send_qu, assoc_recv_qu) in self.queues.iter() {
            while let Some(cmd) = send_qu.poll() {
                let this = this.clone();
                let assoc_recv_qu = assoc_recv_qu.clone();
                crate::tasks::spawn(async move {
                    if let Some(proc) = this.upgrade() {
                        let cmpl = interface::dispatch(&proc, cmd).await;
                        match assoc_recv_qu.post(&cmpl) {
                            Ok(()) => {}
                            Err(_) => {
                                todo!("kill process on queue overflow")
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
        // TODO: the thread needs to go into some kind of dead/error state and/or the process
        // needs to be notified that one of its threads just died. If this is the last thread
        // in the process than the process itself is now dead
    }

    /// Create a future that will resolve with the value of the exit code when this process exits.
    /// If the process is killed or aborted, then [None] will be returned.
    pub fn exit_code(self: &Arc<Process>) -> impl Future<Output = Option<u32>> {
        ProcessExitFuture { proc: self.clone() }
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

struct ProcessExitFuture {
    proc: Arc<Process>,
}

impl Future for ProcessExitFuture {
    type Output = Option<u32>;

    fn poll(
        self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        match self.proc.exit_code.poll() {
            Some(c) => core::task::Poll::Ready(*c),
            None => {
                self.proc.exit_waker.lock().push(cx.waker().clone());
                core::task::Poll::Pending
            }
        }
    }
}

/// Cause a process to exit, freeing its resources.
///
/// Because threads have strong references to their parent process, process resources will not be
/// freed unless this function is called.
// TODO: this should be somewhere else, perhaps with `spawn`.
fn exit_process(proc: Arc<Process>, exit_code: Option<u32>) {
    log::trace!("process {} exited with code {exit_code:?}", proc.id);

    processes().remove(|p| p.id == proc.id);

    // delete and unschedule all threads
    // removing the current thread will automatically schedule a new thread to run
    for thread in proc.threads.lock_blocking().drain(..) {
        scheduler().remove_thread(&thread);
        threads().remove(|t| t.id == thread.id);
        // drop thread
    }

    proc.exit_code.call_once(|| exit_code);
    for w in proc.exit_waker.lock().drain(..) {
        w.wake();
    }

    for mapped_region in proc.page_tables.iter() {
        physical_memory_allocator()
            .free_pages(mapped_region.base_phys_addr, mapped_region.page_count);
    }
}

/// Register system calls related to processes and thread scheduling.
pub fn register_system_call_handlers() {
    use kapi::system_calls::SystemCallNumber;
    let h = crate::exception::system_call_handlers();
    h.insert_blocking(SystemCallNumber::Exit as u16, |_id, proc, _thread, regs| {
        exit_process(proc, Some(regs.x[0] as u32));
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
