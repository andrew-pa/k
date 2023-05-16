use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};

use alloc::{boxed::Box, sync::Arc};

use crate::{
    exception::{self, InterruptId},
    CHashMapG,
};

use super::queue::{Command, Completion, CompletionQueue, QueueId};

pub struct CompletionFuture {
    cmd_id: u16,
    pending_completions: Arc<CHashMapG<u16, PendingCompletion>>,
}

impl Future for CompletionFuture {
    type Output = Completion;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        use PendingCompletion::*;
        let pc = self.pending_completions.remove(&self.cmd_id);
        // TODO: if we recieve an interrupt here and pc == None, will we ever get the
        // completion? probably not
        match pc {
            None => {
                self.pending_completions
                    .insert(self.cmd_id, Waiting(cx.waker().clone()));
                Poll::Pending
            }
            Some(Waiting(_)) => {
                log::warn!("future repolled while waiting");
                self.pending_completions
                    .insert(self.cmd_id, Waiting(cx.waker().clone()));
                Poll::Pending
            }
            Some(Ready(cmp)) => Poll::Ready(cmp),
        }
    }
}

enum PendingCompletion {
    Waiting(Waker),
    Ready(Completion),
}

pub struct CompletionQueueHandle {
    next_cmd_id: u16,
    pending_completions: Arc<CHashMapG<u16, PendingCompletion>>,
    int_id: InterruptId,
}

impl CompletionQueueHandle {
    /// sets the command ID to a unique value, submits the command, and returns a future that
    /// will resolve when the command is completed by the device
    pub fn wait_for_completion<'sq>(&mut self, cmd: Command<'sq>) -> CompletionFuture {
        let cmd_id = self.next_cmd_id;
        self.next_cmd_id = self.next_cmd_id.wrapping_add(1);
        cmd.set_command_id(cmd_id).submit();
        CompletionFuture {
            cmd_id,
            pending_completions: self.pending_completions.clone(),
        }
    }
}

impl Drop for CompletionQueueHandle {
    fn drop(&mut self) {
        // make sure the underlying completion queue gets dropped and that interrupts won't
        // happen that will go unhandled
        // TODO: disable interrupts
        exception::interrupt_handlers().remove(&self.int_id);
    }
}

pub fn register_completion_queue(
    int_id: InterruptId,
    mut qu: CompletionQueue,
) -> CompletionQueueHandle {
    let pending_completions: Arc<CHashMapG<u16, PendingCompletion>> = Arc::new(Default::default());
    {
        let pending_completions = pending_completions.clone();
        exception::interrupt_handlers().insert(int_id, Box::new(move |int_id, _| {
            use PendingCompletion::*;
            if let Some(cmp) = qu.pop() {
                if let Some(mut pc) = pending_completions.get_mut(&cmp.id) {
                    *pc = match &*pc {
                        Waiting(w) => {
                            w.wake_by_ref();
                            Ready(cmp)
                        },
                        Ready(old_cmp) => panic!("recieved second completion {cmp:?} for id with pending ready completion {old_cmp:?}"),
                    }
                } else {
                    pending_completions.insert(cmp.id, Ready(cmp));
                }
            } else {
                // panic here?
                log::error!("got NVMe completion interrupt but the completion queue did not have an available completion (queue {}, int id {int_id})", qu.queue_id());
            }
        }));
    }
    // TODO: enable interrupts
    CompletionQueueHandle {
        next_cmd_id: 0,
        pending_completions,
        int_id,
    }
}