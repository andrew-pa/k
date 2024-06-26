use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};

use alloc::{boxed::Box, sync::Arc};
use smallvec::SmallVec;

use crate::{
    ds::maps::CHashMap,
    exception::{self, InterruptGuard, InterruptId},
};

use super::queue::{Command, Completion, CompletionQueue};

pub struct CompletionFuture {
    cmd_id: u16,
    #[allow(unused)] // we're only holding these to drop them when the future is dropped
    extra_data_ptr_pages_to_drop: SmallVec<[crate::memory::PhysicalBuffer; 1]>,
    pending_completions: Arc<CHashMap<u16, PendingCompletion>>,
}

impl Future for CompletionFuture {
    type Output = Completion;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        use PendingCompletion::*;
        let _ig = InterruptGuard::disable_interrupts_until_drop();
        // log::trace!("polling NVMe completion future for {}", self.cmd_id);
        let pc = self.pending_completions.remove_blocking(&self.cmd_id);
        // TODO: if we recieve an interrupt here and pc == None, will we ever get the
        // completion? probably not
        match pc {
            None => {
                self.pending_completions
                    .insert_blocking(self.cmd_id, Waiting(cx.waker().clone()));
                Poll::Pending
            }
            Some(Waiting(_)) => {
                log::warn!("future repolled while waiting");
                self.pending_completions
                    .insert_blocking(self.cmd_id, Waiting(cx.waker().clone()));
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
    pending_completions: Arc<CHashMap<u16, PendingCompletion>>,
    int_id: InterruptId,
}

impl CompletionQueueHandle {
    /// sets the command ID to a unique value, submits the command, and returns a future that
    /// will resolve when the command is completed by the device
    pub fn wait_for_completion(&mut self, cmd: Command<'_>) -> CompletionFuture {
        let cmd_id = self.next_cmd_id;
        self.next_cmd_id = self.next_cmd_id.wrapping_add(1);
        // log::debug!("created future for NVMe command id {cmd_id}");
        // log::trace!("NVMe command {cmd_id} = {cmd:?}");
        let extra_data_ptr_pages_to_drop = cmd.set_command_id(cmd_id).submit();
        CompletionFuture {
            cmd_id,
            extra_data_ptr_pages_to_drop,
            pending_completions: self.pending_completions.clone(),
        }
    }
}

impl Drop for CompletionQueueHandle {
    fn drop(&mut self) {
        // make sure the underlying completion queue gets dropped and that interrupts won't
        // happen that will go unhandled
        // TODO: disable interrupts
        exception::interrupt_handlers().remove_blocking(&self.int_id);
    }
}

fn handle_interrupt(
    int_id: InterruptId,
    qu: &mut CompletionQueue,
    pending_completions: &Arc<CHashMap<u16, PendingCompletion>>,
) {
    use PendingCompletion::*;
    // log::trace!("handling NVMe interrupt {int_id}, {}", qu.queue_id());
    if let Some(cmp) = qu.pop() {
        // log::debug!(
        //     "pc addr: 0x{:x}",
        //     Arc::as_ptr(pending_completions) as *const _ as usize
        // );
        // log::trace!("completion {cmp:x?}");
        if let Some(mut pc) = pending_completions.get_mut_blocking(&cmp.id) {
            // log::debug!("pending completion?");
            *pc = match &*pc {
                Waiting(w) => {
                    // log::debug!("waking future for NVMe command id {}", cmp.id);
                    w.wake_by_ref();
                    Ready(cmp)
                },
                Ready(old_cmp) => panic!("recieved second completion {cmp:?} for id with pending ready completion {old_cmp:?}"),
            }
        } else {
            // log::trace!("inserting ready completion before future was ever polled");
            pending_completions.insert_blocking(cmp.id, Ready(cmp));
        }
    } else {
        // panic here?
        log::error!("got NVMe completion interrupt but the completion queue did not have an available completion (queue {}, int id {int_id})", qu.queue_id());
    }
}

pub fn enable_interrupts(int_id: InterruptId) {
    let ic = exception::interrupt_controller();
    ic.set_target_cpu(int_id, 0x1);
    ic.set_priority(int_id, 0);
    ic.set_config(int_id, exception::InterruptConfig::Edge);
    ic.clear_pending(int_id);
    ic.set_enable(int_id, true);
}

pub fn register_completion_queue(
    int_id: InterruptId,
    mut qu: CompletionQueue,
) -> CompletionQueueHandle {
    let pending_completions: Arc<CHashMap<u16, PendingCompletion>> = Arc::new(Default::default());
    {
        let pending_completions = pending_completions.clone();
        exception::interrupt_handlers().insert_blocking(
            int_id,
            Box::new(move |int_id, _| handle_interrupt(int_id, &mut qu, &pending_completions)),
        );
    }
    enable_interrupts(int_id);
    CompletionQueueHandle {
        next_cmd_id: 0,
        pending_completions,
        int_id,
    }
}
