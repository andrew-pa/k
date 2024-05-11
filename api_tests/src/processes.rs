use core::ptr::null;

use kapi::{
    commands::{Command, KillProcess, SpawnProcess, WatchProcess},
    completions::{Completion, ErrorCode, Kind as CmplKind, ThreadExit},
    queue::Queue,
    system_calls::{heap_allocate, heap_free, yield_now},
    Buffer, Path, ProcessId,
};

use crate::{wait_for_error_response, Testable};

pub const TESTS: &[&dyn Testable] = &[
    &basic,
    &basic_heap,
    &basic_watch,
    &basic_kill,
    &fail_binary_not_found,
    &fail_invalid_path_ptr,
    &fail_invalid_parameter_ptr,
    &fail_to_kill_bad_id,
];

fn basic(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(Command {
            id: 0,
            kind: SpawnProcess {
                binary_path: Path::from("/volumes/root/bin/test_process"),
                parameters: Buffer::empty(),
                send_completion_on_main_thread_exit: true,
            }
            .into(),
        })
        .expect("send spawn process");

    let pid = wait_for_result_value!(recv_qu, 0, CmplKind::NewProcess(np) => np.id);
    log::info!("spawned process {pid}");

    // wait for the process to exit
    wait_for_result_value!(recv_qu, 0, CmplKind::ThreadExit(ThreadExit::Normal(0)) => ());
}

fn basic_watch(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(Command {
            id: 0,
            kind: SpawnProcess {
                binary_path: Path::from("/volumes/root/bin/test_process"),
                parameters: Buffer::from(&[42u8] as &[u8]),
                send_completion_on_main_thread_exit: true,
            }
            .into(),
        })
        .expect("send spawn process");

    let pid = wait_for_result_value!(recv_qu, 0, CmplKind::NewProcess(np) => np.id);

    send_qu
        .post(Command {
            id: 1,
            kind: WatchProcess { process_id: pid }.into(),
        })
        .expect("send watch process");
    send_qu
        .post(Command {
            id: 2,
            kind: KillProcess { process_id: pid }.into(),
        })
        .expect("send kill process");

    // wait for the process to exit
    let mut outstanding = 3;
    while outstanding > 0 {
        if let Some(c) = recv_qu.poll() {
            match c {
                Completion {
                    response_to_id: 0,
                    kind: CmplKind::ThreadExit(ThreadExit::Killed),
                } => {
                    log::trace!("got thread exit");
                    outstanding -= 1;
                }
                Completion {
                    response_to_id: 1,
                    kind: CmplKind::ThreadExit(ThreadExit::Killed),
                } => {
                    log::trace!("got thread exit from watch");
                    outstanding -= 1;
                }
                Completion {
                    response_to_id: 2,
                    kind: CmplKind::Success,
                } => {
                    log::trace!("got kill completion");
                    outstanding -= 1;
                }
                _ => panic!("unexpected completion: {c:?}"),
            }
        }
        yield_now();
    }
}

fn fail_binary_not_found(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(Command {
            id: 0,
            kind: SpawnProcess {
                binary_path: Path::from("/dne"),
                parameters: Buffer::empty(),
                send_completion_on_main_thread_exit: false,
            }
            .into(),
        })
        .expect("send spawn process");

    wait_for_error_response(recv_qu, 0, ErrorCode::NotFound);
}

fn fail_invalid_path_ptr(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(Command {
            id: 0,
            kind: SpawnProcess {
                binary_path: Path {
                    text: null(),
                    len: 1000,
                },
                parameters: Buffer::empty(),
                send_completion_on_main_thread_exit: false,
            }
            .into(),
        })
        .expect("send spawn process");

    wait_for_error_response(recv_qu, 0, ErrorCode::InvalidPointer);
}

fn fail_invalid_parameter_ptr(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(Command {
            id: 0,
            kind: SpawnProcess {
                binary_path: Path::from("/volumes/root/bin/test_process"),
                parameters: Buffer {
                    data: 0x0000_aaaa_bbbb_cccc as *const u8,
                    len: 1004,
                },
                send_completion_on_main_thread_exit: false,
            }
            .into(),
        })
        .expect("send spawn process");

    wait_for_error_response(recv_qu, 0, ErrorCode::InvalidPointer);
}

fn basic_kill(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(Command {
            id: 0,
            kind: SpawnProcess {
                binary_path: Path::from("/volumes/root/bin/test_process"),
                parameters: Buffer::from(&[42u8] as &[u8]),
                send_completion_on_main_thread_exit: true,
            }
            .into(),
        })
        .expect("send spawn process");

    let pid = wait_for_result_value!(recv_qu, 0, CmplKind::NewProcess(np) => np.id);

    send_qu
        .post(Command {
            id: 1,
            kind: KillProcess { process_id: pid }.into(),
        })
        .expect("send kill process");

    // wait for the process to exit
    let mut outstanding = 2;
    while outstanding > 0 {
        if let Some(c) = recv_qu.poll() {
            match c {
                Completion {
                    response_to_id: 0,
                    kind: CmplKind::ThreadExit(ThreadExit::Killed),
                } => {
                    log::trace!("got thread exit");
                    outstanding -= 1;
                }
                Completion {
                    response_to_id: 1,
                    kind: CmplKind::Success,
                } => {
                    log::trace!("got kill completion");
                    outstanding -= 1;
                }
                _ => panic!("unexpected completion: {c:?}"),
            }
        }
        yield_now();
    }
}

fn fail_to_kill_bad_id(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(Command {
            id: 0,
            kind: KillProcess {
                process_id: ProcessId::new(293492034).unwrap(),
            }
            .into(),
        })
        .expect("send kill process");

    wait_for_error_response(recv_qu, 0, ErrorCode::InvalidId);
}

fn basic_heap(_send_qu: &Queue<Command>, _recv_qu: &Queue<Completion>) {
    let (p, len) = heap_allocate(8192);
    let p = p as *mut u8;

    assert!(len >= 8192);

    // make sure the whole range is actually accessable
    assert!(!p.is_null());
    for i in 0..len {
        unsafe {
            p.add(i).write((i % 256) as u8);
        }
    }

    heap_free(p as *mut (), len);
}
