use kapi::{
    commands::{self as cmds, Kind as CmdKind},
    completions::{self as cmpl, ErrorCode, Kind as CmplKind},
    queue::QueueId,
};

use crate::process::*;

impl Process {
    fn create_completion_queue(
        &self,
        info: &cmds::CreateCompletionQueue,
    ) -> Result<cmpl::NewQueue, ErrorCode> {
        let cmds::CreateCompletionQueue { size } = info;
        let id = QueueId::new(
            self.next_queue_id
                .fetch_add(1, core::sync::atomic::Ordering::Acquire),
        )
        .unwrap();
        // TODO: error handling moment
        let qu = OwnedQueue::<Completion>::new(id, *size).expect("TODO");
        Ok(cmpl::NewQueue {
            id,
            start: todo!(),
            size_in_bytes: todo!(),
        })
    }
}

async fn dispatch_inner(proc: &Arc<Process>, cmd: Command) -> Result<CmplKind, ErrorCode> {
    match cmd.kind {
        CmdKind::Test(cmds::Test { arg }) => Ok(cmpl::Test { arg, pid: proc.id }.into()),
        CmdKind::CreateCompletionQueue(info) => proc.create_completion_queue(&info).map(Into::into),
        /*Kind::SpawnProcess => {
            let path_bytes = unsafe {
                core::slice::from_raw_parts(cmd.args[0] as *const u8, cmd.args[1] as usize)
            };
            // TODO: should we copy the path string into the kernel heap?
            let path =
                Path::new(core::str::from_utf8(path_bytes).expect("TODO: graceful error handling"));
            match spawn_process(path, None::<fn(_)>).await {
                Ok(proc) => Completion {
                    status: SuccessCode::Success.into(),
                    response_to_id: cmd.id,
                    result0: proc.id.into(),
                    data: [0; 3],
                },
                Err(e) => {
                    log::error!("process {}: failed to spawn process: {e}", proc.id);
                    // TODO: compute error code
                    todo!()
                }
            }
        }*/
        kind => {
            log::error!("received unknown command: {}", kind.discriminant());
            Err(ErrorCode::UnknownCommand)
        }
    }
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
