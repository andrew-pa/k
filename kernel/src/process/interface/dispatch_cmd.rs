use kapi::{
    commands::{self as cmds, Kind as CmdKind},
    completions::{self as cmpl, ErrorCode, Kind as CmplKind},
    queue::QueueId,
};

use crate::{
    error::{self, Error},
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
}

async fn dispatch_inner(proc: &Arc<Process>, cmd: Command) -> Result<CmplKind, ErrorCode> {
    match &cmd.kind {
        CmdKind::Test(cmds::Test { arg }) => Ok(cmpl::Test {
            arg: *arg,
            pid: proc.id,
        }
        .into()),
        CmdKind::CreateCompletionQueue(info) => {
            proc.create_completion_queue(info).await.map(Into::into)
        }
        CmdKind::CreateSubmissionQueue(info) => {
            proc.create_submission_queue(info).await.map(Into::into)
        }
        CmdKind::DestroyQueue(info) => proc.destroy_queue(info).map(Into::into),
        kind => Err(Error::Misc {
            reason: alloc::format!("received unknown command: {}", kind.discriminant()),
            code: Some(ErrorCode::UnknownCommand),
        }),
    }
    .map_err(|e| {
        log::error!(
            "[pid {}] error occurred processing {cmd:?}: {}",
            proc.id,
            snafu::Report::from_error(&e)
        );
        e.as_code()
    })
}

pub async fn dispatch(proc: &Arc<Process>, cmd: Command) -> Completion {
    let response_to_id = cmd.id;
    let kind = match dispatch_inner(proc, cmd).await {
        Ok(c) => c,
        Err(e) => e.into(),
    };
    Completion {
        response_to_id,
        kind,
    }
}
