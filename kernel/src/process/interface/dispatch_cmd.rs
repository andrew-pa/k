use alloc::sync::Weak;
use futures::FutureExt;
use kapi::{
    commands::{self as cmds, Kind as CmdKind},
    completions::{self as cmpl, ErrorCode, Kind as CmplKind},
    queue::QueueId,
};
use snafu::ensure;

use crate::{
    error::{self, Error, InnerSnafu, MemorySnafu},
    process::*,
};

impl Process {
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
                    code: Some(ErrorCode::OutOfIds),
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

    async fn create_queue<T>(
        &self,
        num_elements: usize,
    ) -> Result<(OwnedQueue<T>, cmpl::NewQueue), Error> {
        if num_elements == 0 {
            return Err(Error::Misc {
                reason: "queue must have at least a capacity of 1, got 0".into(),
                code: Some(ErrorCode::InvalidSize),
            });
        }

        let id = self.next_queue_id()?;
        let qu = OwnedQueue::<T>::new(id, num_elements).context(error::MemorySnafu {
            reason: "allocate backing memory for queue",
        })?;
        let addr = self
            .address_space_allocator
            .lock()
            .await
            .alloc(qu.buffer.page_count())
            .context(error::MemorySnafu {
                reason: "allocate address space for queue",
            })?;
        self.page_tables
            .map_range(
                qu.buffer.physical_address(),
                addr,
                qu.buffer.page_count(),
                true,
                &PageTableEntryOptions {
                    read_only: false,
                    el0_access: true,
                },
            )
            .context(error::MemorySnafu {
                reason: "map queue into process address space",
            })?;
        Ok((qu, cmpl::NewQueue { id, start: addr.0 }))
    }

    async fn create_completion_queue(
        &self,
        info: &cmds::CreateCompletionQueue,
    ) -> Result<cmpl::NewQueue, Error> {
        let (oq, nq) = self.create_queue::<Completion>(info.size).await?;
        self.recv_queues.push(Arc::new(oq));
        Ok(nq)
    }

    async fn create_submission_queue(
        &self,
        info: &cmds::CreateSubmissionQueue,
    ) -> Result<cmpl::NewQueue, Error> {
        let rq = self
            .recv_queues
            .iter()
            .find(|q| q.id == info.associated_completion_queue)
            .cloned()
            .with_context(|| error::MiscSnafu {
                reason: alloc::format!(
                    "completion queue {} not found",
                    info.associated_completion_queue
                ),
                code: Some(ErrorCode::InvalidId),
            })?;
        let (oq, nq) = self.create_queue::<Command>(info.size).await?;
        self.send_queues.push((Arc::new(oq), rq));
        Ok(nq)
    }

    fn destroy_queue(&self, info: &cmds::DestroyQueue) -> Result<cmpl::Success, Error> {
        if self.send_queues.iter().any(|(_, q)| q.id == info.id) {
            Err(Error::Misc {
                reason: "completion queue still in use".into(),
                code: Some(ErrorCode::InUse),
            })
        } else {
            self.recv_queues.remove(|q| q.id == info.id);
            self.send_queues.remove(|(q, _)| q.id == info.id);
            Ok(cmpl::Success)
        }
    }

    /// Map more memory into the process. This memory may not be physically continuous, but will be
    /// virtually continuous in the process's address space.
    async fn alloc_memory(&self, mut page_count: usize) -> Result<VirtualAddress, Error> {
        let vaddr = self
            .address_space_allocator
            .lock()
            .await
            .alloc(page_count)
            .context(MemorySnafu {
                reason: "allocate virtual addresses for memory region in process",
            })?;
        let mut vstart = vaddr;
        let options = PageTableEntryOptions {
            read_only: false,
            el0_access: true,
        };
        while page_count > 0 {
            let (paddr, size) = physical_memory_allocator()
                .try_alloc_contig(page_count)
                .context(MemorySnafu {
                    reason: "allocate physical memory for process",
                })?;
            self.page_tables
                .map_range(paddr, vstart, size, true, &options)
                .context(MemorySnafu {
                    reason: "map physical memory into process address space",
                })?;
            page_count -= size;
            vstart = vstart.add(size);
        }
        Ok(vaddr)
    }

    async fn spawn_thread(
        self: &Arc<Process>,
        info: &cmds::SpawnThread,
        cmd_id: u16,
        recv_qu: Weak<OwnedQueue<Completion>>,
    ) -> Result<cmpl::NewThread, Error> {
        ensure!(
            info.stack_size > 0,
            error::MiscSnafu {
                code: Some(ErrorCode::InvalidSize),
                reason: "thread stack must have non-zero size"
            }
        );

        let stack_page_count = info.stack_size.div_ceil(PAGE_SIZE);

        let initial_stack_pointer =
            self.alloc_memory(stack_page_count)
                .await
                .context(InnerSnafu {
                    reason: "allocate thread stack",
                })?;

        let tid = thread::next_thread_id();
        let thread = Arc::new(Thread::user_thread(
            self.clone(),
            tid,
            VirtualAddress(info.entry_point as usize),
            initial_stack_pointer,
            ThreadPriority::Normal,
            Registers::from_args(&[info.user_data as usize]),
        ));

        if info.send_completion_on_exit {
            crate::tasks::spawn(thread.exit_code().map(move |ec| {
                if let Some(rq) = recv_qu.upgrade() {
                    // TODO: deal with queue overflow
                    let _ = rq.post(Completion {
                        response_to_id: cmd_id,
                        kind: ec.into(),
                    });
                }
            }));
        }

        self.threads.lock().await.push(thread.clone());
        thread::spawn_thread(thread);

        Ok(cmpl::NewThread { id: tid })
    }

    async fn watch_thread(
        self: &Arc<Process>,
        info: &cmds::WatchThread,
    ) -> Result<cmpl::ThreadExit, Error> {
        let thread = thread_for_id(info.thread_id).context(error::MiscSnafu {
            reason: "find thread to watch",
            code: Some(ErrorCode::InvalidId),
        })?;

        Ok(thread.exit_code().await)
    }

    /// Execute a single command on the process, returning the resulting completion.
    pub async fn dispatch_command(
        self: &Arc<Process>,
        cmd: Command,
        recv_qu: Weak<OwnedQueue<Completion>>,
    ) -> Completion {
        let res = match &cmd.kind {
            CmdKind::Test(cmds::Test { arg }) => Ok(cmpl::Test {
                arg: *arg,
                pid: self.id,
            }
            .into()),
            CmdKind::CreateCompletionQueue(info) => {
                self.create_completion_queue(info).await.map(Into::into)
            }
            CmdKind::CreateSubmissionQueue(info) => {
                self.create_submission_queue(info).await.map(Into::into)
            }
            CmdKind::DestroyQueue(info) => self.destroy_queue(info).map(Into::into),
            CmdKind::SpawnThread(info) => self
                .spawn_thread(info, cmd.id, recv_qu)
                .await
                .map(Into::into),
            CmdKind::WatchThread(info) => self.watch_thread(info).await.map(Into::into),
            kind => Err(Error::Misc {
                reason: alloc::format!("received unknown command: {}", kind.discriminant()),
                code: Some(ErrorCode::UnknownCommand),
            }),
        };

        let kind = res.unwrap_or_else(|e| {
            log::error!(
                "[pid {}] error occurred processing {cmd:?}: {}",
                self.id,
                snafu::Report::from_error(&e)
            );
            CmplKind::Err(e.as_code())
        });

        Completion {
            response_to_id: cmd.id,
            kind,
        }
    }
}
