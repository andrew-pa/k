use core::{
    ptr::{addr_of, addr_of_mut, null_mut},
    sync::atomic::AtomicBool,
};

use kapi::{
    commands::{Command, SpawnThread, WatchThread},
    completions::{Completion, ErrorCode, Kind as CmplKind, ThreadExit},
    queue::Queue,
    system_calls::{exit, yield_now},
    ThreadId,
};

use crate::Testable;

pub const TESTS: &[&dyn Testable] = &[
    &basic,
    &basic_watch,
    &fail_to_create_thread_with_no_stack,
    &thread_with_bad_entry_point_page_faults,
];

fn basic_thread_entry(user_data: *mut ()) -> ! {
    unsafe {
        let data = user_data as *mut i32;
        assert!(!data.is_null());
        assert_eq!(data.read(), 12345678, "check user data pointer");
        data.write(87654321);
    }
    log::trace!("hello from thread!");
    exit(7)
}

fn basic(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    let mut data = 12345678i32;

    send_qu
        .post(Command {
            id: 0,
            kind: SpawnThread {
                entry_point: basic_thread_entry,
                stack_size: 4096,
                user_data: addr_of_mut!(data) as *mut (),
                send_completion_on_exit: true,
            }
            .into(),
        })
        .expect("spawn thread");

    let mut tid: Option<ThreadId> = None;
    while tid.is_none() {
        if let Some(c) = recv_qu.poll() {
            assert_eq!(c.response_to_id, 0);
            match c.kind {
                CmplKind::NewThread(nt) => {
                    tid = Some(nt.id);
                }
                _ => panic!("unexpected completion: {c:?}"),
            }
        }
        yield_now();
    }

    let tid = tid.unwrap();
    log::debug!("created thread {tid}");

    loop {
        if let Some(c) = recv_qu.poll() {
            assert_eq!(c.response_to_id, 0);
            assert_eq!(c.kind, CmplKind::ThreadExit(ThreadExit::Normal(7)));
            break;
        }
        yield_now();
    }

    assert_eq!(data, 87654321, "check thread modified user data");
}

fn exit_on_signal(signal_raw: *mut ()) -> ! {
    let signal = unsafe { (signal_raw as *mut AtomicBool).as_ref().unwrap() };
    loop {
        if signal.load(core::sync::atomic::Ordering::Acquire) {
            break;
        }
    }
    exit(7)
}

fn basic_watch(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    let signal = AtomicBool::new(false);

    send_qu
        .post(Command {
            id: 0,
            kind: SpawnThread {
                entry_point: exit_on_signal,
                stack_size: 4096,
                user_data: addr_of!(signal) as *mut (),
                send_completion_on_exit: true,
            }
            .into(),
        })
        .expect("spawn thread");

    let mut tid: Option<ThreadId> = None;
    while tid.is_none() {
        if let Some(c) = recv_qu.poll() {
            assert_eq!(c.response_to_id, 0);
            match c.kind {
                CmplKind::NewThread(nt) => {
                    tid = Some(nt.id);
                }
                _ => panic!("unexpected completion: {c:?}"),
            }
        }
        yield_now();
    }

    let tid = tid.unwrap();
    log::debug!("created thread {tid}");

    send_qu
        .post(Command {
            id: 1,
            kind: WatchThread { thread_id: tid }.into(),
        })
        .expect("watch thread");

    yield_now();

    signal.store(true, core::sync::atomic::Ordering::Release);

    let mut outstanding = 2;
    while outstanding > 0 {
        if let Some(c) = recv_qu.poll() {
            assert!(c.response_to_id == 0 || c.response_to_id == 1);
            match c.kind {
                CmplKind::ThreadExit(ThreadExit::Normal(c)) => {
                    assert_eq!(c, 7);
                    outstanding -= 1;
                }
                _ => panic!("unexpected completion: {c:?}"),
            }
        }
        yield_now();
    }
}

fn fail_to_create_thread_with_no_stack(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(Command {
            id: 1,
            kind: SpawnThread {
                entry_point: exit_on_signal,
                stack_size: 0,
                user_data: null_mut(),
                send_completion_on_exit: false,
            }
            .into(),
        })
        .expect("post create queue msg");

    // wait for the error to come back
    loop {
        if let Some(c) = recv_qu.poll() {
            assert_eq!(c.response_to_id, 1);
            match c.kind {
                CmplKind::Err(ErrorCode::InvalidSize) => return,
                _ => panic!("unexpected completion: {c:?}"),
            }
        }
        yield_now();
    }
}

fn thread_with_bad_entry_point_page_faults(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    let nasty_entry_point: fn(*mut ()) -> ! = unsafe { core::mem::transmute(1usize) };

    send_qu
        .post(Command {
            id: 0,
            kind: SpawnThread {
                entry_point: nasty_entry_point,
                stack_size: 4096,
                user_data: null_mut(),
                send_completion_on_exit: true,
            }
            .into(),
        })
        .expect("post create queue msg");

    let mut tid: Option<ThreadId> = None;
    while tid.is_none() {
        if let Some(c) = recv_qu.poll() {
            assert_eq!(c.response_to_id, 0);
            match c.kind {
                CmplKind::NewThread(nt) => {
                    tid = Some(nt.id);
                }
                _ => panic!("unexpected completion: {c:?}"),
            }
        }
        yield_now();
    }

    // wait for the thread to page fault
    loop {
        if let Some(c) = recv_qu.poll() {
            assert_eq!(c.response_to_id, 0);
            match c.kind {
                CmplKind::ThreadExit(ThreadExit::PageFault) => return,
                _ => panic!("unexpected completion: {c:?}"),
            }
        }
        yield_now();
    }
}
