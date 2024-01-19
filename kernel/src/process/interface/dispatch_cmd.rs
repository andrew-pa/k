use crate::process::*;
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
        CommandKind::Reserved(kind) => Completion {
            kind: CompletionKind::UnknownCommand,
            response_to_id: cmd.id,
            result0: kind as u32,
            result1: tid as u64,
        },
    }
}
