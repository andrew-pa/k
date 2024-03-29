use crate::{process::*, registry::Path};
use kapi::*;

// TODO: refactor into a function that returns Result<Completion, Error> and change this function
// so that it calls that function and then on errors calls some Error -> Completion function
pub async fn dispatch(proc: &Arc<Process>, cmd: Command) -> Completion {
    log::debug!("received from pid {}: {cmd:?}", proc.id);
    match cmd.kind {
        CommandKind::Invalid => todo!(),
        CommandKind::Test => Completion {
            status: SuccessCode::Success.into(),
            response_to_id: cmd.id,
            result0: proc.id.into(),
            result1: cmd.args[0],
        },
        CommandKind::SpawnProcess => {
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
                    result1: 0,
                },
                Err(e) => {
                    log::error!("process {}: failed to spawn process: {e}", proc.id);
                    // TODO: compute error code
                    todo!()
                }
            }
        }
        CommandKind::Reserved(kind) => Completion {
            status: ErrorCode::UnknownCommand.into(),
            response_to_id: cmd.id,
            result0: kind as u32,
            result1: 0,
        },
    }
}
