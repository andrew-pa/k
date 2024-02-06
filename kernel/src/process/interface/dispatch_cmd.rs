use crate::{process::*, registry::Path};
use kapi::*;

pub async fn dispatch(pid: ProcessId, tid: ThreadId, cmd: Command) -> Completion {
    match cmd.kind {
        CommandKind::Invalid => todo!(),
        CommandKind::Test => Completion {
            kind: CompletionKind::Success,
            response_to_id: cmd.id,
            result0: pid,
            result1: tid as u64,
        },
        CommandKind::SpawnProcess => {
            let path_bytes = unsafe {
                core::slice::from_raw_parts(cmd.args[0] as *const u8, cmd.args[1] as usize)
            };
            // TODO: should we copy the path string into the kernel heap?
            let path =
                Path::new(core::str::from_utf8(path_bytes).expect("TODO: graceful error handling"));
            match spawn_process(path, None::<fn(&mut Process)>).await {
                Ok(pid) => Completion {
                    kind: CompletionKind::Success,
                    response_to_id: cmd.id,
                    result0: pid,
                    result1: 0,
                },
                Err(e) => {
                    log::error!("process {pid}: failed to spawn process: {e}");
                    // TODO: compute error code
                    Completion {
                        kind: CompletionKind::Error,
                        response_to_id: cmd.id,
                        result0: 0,
                        result1: 0,
                    }
                }
            }
        }
        CommandKind::Reserved(kind) => Completion {
            kind: CompletionKind::UnknownCommand,
            response_to_id: cmd.id,
            result0: kind as u32,
            result1: tid as u64,
        },
    }
}
