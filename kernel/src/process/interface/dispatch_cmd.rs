use bytemuck::Zeroable;
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

    fn create_queue<T: Zeroable>(&self, num_elements: usize) -> Result<cmpl::NewQueue, Error> {
        let id = self.next_queue_id()?;
        let qu = OwnedQueue::<T>::new(id, num_elements).context(error::MemorySnafu {
            reason: "allocate backing memory for queue",
        })?;
        let addr = self
            .address_space_allocator
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
        Ok(cmpl::NewQueue { id, start: addr.0 })
    }

    fn create_completion_queue(
        &self,
        info: &cmds::CreateCompletionQueue,
    ) -> Result<cmpl::NewQueue, Error> {
        self.create_queue::<Completion>(info.size)
    }
}

async fn dispatch_inner(proc: &Arc<Process>, cmd: Command) -> Result<CmplKind, ErrorCode> {
    match &cmd.kind {
        CmdKind::Test(cmds::Test { arg }) => Ok(cmpl::Test {
            arg: *arg,
            pid: proc.id,
        }
        .into()),
        CmdKind::CreateCompletionQueue(info) => proc.create_completion_queue(&info).map(Into::into),
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
    log::trace!("received from pid {}: {cmd:?}", proc.id);
    let response_to_id = cmd.id;
    let kind = match dispatch_inner(proc, cmd).await {
        Ok(c) => c,
        Err(e) => e.into(),
    };
    log::trace!("sending completion for cmd #{response_to_id}: {kind:?}");
    Completion {
        response_to_id,
        kind,
    }
}
