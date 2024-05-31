//! User-space, with scheduled [threads][Thread] grouped into [processes][Process].
//! Contains the thread scheduler and system message dispatch infrastructure.
use crate::{
    ds::lists::ConcurrentLinkedList,
    error::Error,
    exception::Registers,
    memory::{
        self,
        paging::{PageTable, PageTableEntryOptions},
        physical_memory_allocator, virtual_address_allocator, PhysicalBuffer, VirtualAddress,
        VirtualAddressAllocator, PAGE_SIZE,
    },
    tasks::locks::Mutex,
};
use alloc::{boxed::Box, sync::Arc};

use bitfield::Bit;
use core::{
    num::NonZeroU32,
    sync::atomic::{AtomicU16, AtomicU32},
};
use futures::Future;
use kapi::{
    commands::Command,
    completions::Completion,
    queue::{Queue, QueueId},
    FileHandle,
};
use smallvec::SmallVec;
use snafu::{OptionExt, ResultExt};
use spin::Once;

pub mod thread;
use thread::*;
pub use thread::{scheduler::scheduler, threads, Thread, ThreadId};

mod commands;

mod spawn;
pub use spawn::spawn_process;

pub use kapi::ProcessId;

/// A struct to manage an observable thread exit state.
#[derive(Default)]
struct ExitState {
    /// The exit code this process/thread exited with (if it has exited).
    code: Once<kapi::completions::ThreadExit>,
    /// Future wakers for futures waiting on exit code.
    wakers: spin::Mutex<SmallVec<[core::task::Waker; 1]>>,
}

impl ExitState {
    fn set(&self, val: kapi::completions::ThreadExit) {
        self.code.call_once(|| val);
        for w in self.wakers.lock().drain(..) {
            w.wake();
        }
    }
}

/// A future that resolves when an [ExitState] gets set.
struct ExitFuture {
    state: Arc<ExitState>,
}

impl Future for ExitFuture {
    type Output = kapi::completions::ThreadExit;

    fn poll(
        self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        match self.state.code.poll() {
            Some(c) => core::task::Poll::Ready(c.clone()),
            None => {
                self.state.wakers.lock().push(cx.waker().clone());
                core::task::Poll::Pending
            }
        }
    }
}

/// A single synchronized queue that is shared with a process.
pub struct UserQueue<T> {
    queue: Queue<T>,
    user_base_address: VirtualAddress,
}

impl<T> UserQueue<T> {
    /// Returns the user and kernel addresses of the start of the queue, as well as the number of pages that the queue takes up.
    fn memory_region(&self) -> (VirtualAddress, VirtualAddress, usize) {
        (
            self.user_base_address,
            self.root_address().as_ptr().into(),
            kapi::queue::queue_size_in_bytes::<T>(self.capacity()).div_ceil(PAGE_SIZE),
        )
    }
}

impl<T> core::ops::Deref for UserQueue<T> {
    type Target = Queue<T>;

    fn deref(&self) -> &Self::Target {
        &self.queue
    }
}

impl<T> core::fmt::Debug for UserQueue<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OwnedQueue")
            .field("id", &self.queue.id)
            .field("capacity", &self.queue.capacity())
            .finish()
    }
}

crate::assert_send!(kapi::commands::Command);
crate::assert_send!(kapi::completions::Completion);

pub struct OpenFile {
    handle: FileHandle,
    file: Mutex<Box<dyn crate::fs::File>>,
}

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
    next_queue_id: AtomicU16,
    /// The send (user space to kernel) queues associated with this process, and their associated
    /// receive queue.
    #[allow(clippy::type_complexity)]
    send_queues: ConcurrentLinkedList<(Arc<UserQueue<Command>>, Arc<UserQueue<Completion>>)>,
    /// The receive (kernel to user space) queues associated with this process.
    // TODO: these queues don't need to be owned, because the process implicitly owns
    // all the mapped memory in its page tables by default.
    recv_queues: ConcurrentLinkedList<Arc<UserQueue<Completion>>>,

    /// The exit state for the whole process, which will be the same as the *last* thread to exit
    /// in the process.
    exit_state: Arc<ExitState>,

    /// The next free file handle value.
    next_handle: AtomicU32,
    /// The files currently opened by this process.
    open_files: ConcurrentLinkedList<Arc<OpenFile>>,
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

    /// Create a future that will resolve with the value of the exit code when the last thread in
    /// the process exits.
    pub fn exit_code(&self) -> impl Future<Output = kapi::completions::ThreadExit> {
        ExitFuture {
            state: self.exit_state.clone(),
        }
    }

    /// Map more memory into the process.
    /// This memory may not be physically continuous, but will be virtually continuous.
    ///
    /// If `share_with_kernel` is true, then the memory will also be mapped into the kernel's
    /// address space and the kernel base address will be returned as the second value.
    // TODO: zero pages?
    async fn alloc_memory(
        &self,
        mut page_count: usize,
        share_with_kernel: bool,
    ) -> Result<(VirtualAddress, Option<VirtualAddress>), Error> {
        use crate::error::*;
        let vaddr = self
            .address_space_allocator
            .lock()
            .await
            .alloc(page_count)
            .context(MemorySnafu {
                reason: "allocate virtual addresses for memory region in process",
            })?;
        let kvaddr = share_with_kernel
            .then(|| virtual_address_allocator().alloc(page_count))
            .transpose()
            .context(MemorySnafu {
                reason: "allocate virtual addresses for memory region in kernel",
            })?;
        let options = PageTableEntryOptions {
            read_only: false,
            el0_access: true,
        };
        let mut offset = 0;
        while page_count > 0 {
            let (paddr, size) = physical_memory_allocator()
                .try_alloc_contig(page_count)
                .context(MemorySnafu {
                    reason: "allocate physical memory for process",
                })?;
            self.page_tables
                .map_range(paddr, vaddr.add(offset), size, true, &options)
                .context(MemorySnafu {
                    reason: "map new memory into process address space",
                })?;
            if let Some(kvaddr) = kvaddr.as_ref() {
                memory::paging::kernel_table()
                    .map_range(paddr, kvaddr.add(offset), size, true, &options)
                    .context(MemorySnafu {
                        reason: "map new memory into kernel address space",
                    })?
            }
            offset += size * PAGE_SIZE;
            page_count -= size;
        }
        log::trace!("allocated {offset} bytes at {vaddr} ({kvaddr:?})");
        Ok((vaddr, kvaddr))
    }

    /// Free memory allocated by [Self::alloc_memory], both physical and virtual.
    ///
    /// This function does *not* unmap the memory from the kernel address space if the region was
    /// allocated with `share_with_kernel` set to true!
    fn free_memory(&self, ptr: VirtualAddress, page_count: usize) {
        log::trace!("freeing {page_count} pages at {ptr}");
        self.page_tables
            .unmap_range_custom(ptr, page_count, |region| {
                physical_memory_allocator().free_pages(region.base_phys_addr, region.page_count);
            });
        self.address_space_allocator
            .lock_blocking()
            .free(ptr, page_count);
    }

    /// Get the next free queue ID in this process.
    ///
    /// If all the queue IDs have already been used, an error is returned.
    fn next_queue_id(&self) -> Result<QueueId, Error> {
        // the complexity here is because once `next_queue_id` becomes zero, it needs to stay zero
        // to prevent reusing IDs.
        let mut id = self
            .next_queue_id
            .load(core::sync::atomic::Ordering::Acquire);
        loop {
            if id == 0 {
                return Err(Error::Misc {
                    reason: "ran out of queue IDs for process".into(),
                    code: Some(kapi::completions::ErrorCode::OutOfIds),
                });
            }
            match self.next_queue_id.compare_exchange_weak(
                id,
                id.wrapping_add(1),
                core::sync::atomic::Ordering::Acquire,
                core::sync::atomic::Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Ok(unsafe {
                        // SAFETY: we just checked `id` to see if it was zero above.
                        QueueId::new_unchecked(id)
                    });
                }
                Err(x) => id = x,
            }
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // clean up the kernel mapping for all the queues in this process
        // it's possible (but unlikely) that the queues could get used after this, but it will result in a page
        // fault if it does happen.
        for (qu, _) in self.send_queues.iter() {
            let (_, addr, page_count) = qu.memory_region();
            virtual_address_allocator().free(addr, page_count);
        }

        for qu in self.recv_queues.iter() {
            let (_, addr, page_count) = qu.memory_region();
            virtual_address_allocator().free(addr, page_count);
        }

        for mapped_region in self.page_tables.iter() {
            physical_memory_allocator()
                .free_pages(mapped_region.base_phys_addr, mapped_region.page_count);
        }
    }
}

/// The global process table.
///
/// Processes that are in the table are "live" and can be referred to using their ID by user-space.
static PROCESSES: Once<ConcurrentLinkedList<Arc<Process>>> = Once::new();

/// The global PID counter.
static NEXT_PID: AtomicU32 = AtomicU32::new(1);

/// The global table of processes by ID.
pub fn processes() -> &'static ConcurrentLinkedList<Arc<Process>> {
    PROCESSES.call_once(Default::default)
}

/// Retrieve a process by its ID.
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
        SystemCallNumber::GetCurrentProcessId as u16,
        |_id, proc, _thread, regs| {
            // TODO: validate pointer
            let p = regs.x[0] as *mut NonZeroU32;
            unsafe {
                p.write(proc.id);
            }
        },
    );

    h.insert_blocking(
        SystemCallNumber::GetCurrentThreadId as u16,
        |_id, _proc, thread, regs| {
            // TODO: validate pointer
            let p = regs.x[0] as *mut u32;
            unsafe {
                p.write(thread.id);
            }
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

    h.insert_blocking(
        SystemCallNumber::HeapAllocate as u16,
        |_id, proc, thread, regs| {
            let page_count = regs.x[0].div_ceil(PAGE_SIZE);
            let res_ptr = regs.x[1] as *mut VirtualAddress;
            let res_size = regs.x[2] as *mut usize;
            crate::tasks::block_on(async move {
                // log::trace!("{res_ptr:?},{res_size:?} .. {page_count}");
                match proc.alloc_memory(page_count, false).await {
                    Ok((addr, _)) => unsafe {
                        res_ptr.write(addr);
                        res_size.write(page_count * PAGE_SIZE);
                    },
                    Err(e) => {
                        log::error!(
                            "[pid {}, tid {}] error allocating heap memory:\n{}",
                            proc.id,
                            thread.id,
                            snafu::Report::from_error(&e)
                        );
                        unsafe {
                            res_ptr.write(VirtualAddress(0));
                            res_size.write(0);
                        }
                    }
                }
            })
        },
    );

    h.insert_blocking(
        SystemCallNumber::HeapFree as u16,
        |_id, proc, _thread, regs| {
            let ptr = VirtualAddress(regs.x[0]);
            let size = regs.x[1].div_ceil(PAGE_SIZE);
            proc.free_memory(ptr, size);
        },
    );
}
