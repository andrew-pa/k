use kapi::{
    commands::{Command, CreateCompletionQueue, CreateSubmissionQueue, DestroyQueue},
    completions::{Completion, ErrorCode, Kind as CmplKind},
    queue::{Queue, QueueId},
    system_calls::yield_now,
};

use crate::Testable;

pub const TESTS: &[&dyn Testable] = &[
    &basic_create_destroy,
    &fail_to_create_submission_queue_with_bad_completion_id,
    &fail_to_create_queue_with_zero_size,
];

fn basic_create_destroy(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    // create a completion queue
    send_qu
        .post(&Command {
            id: 0,
            kind: CreateCompletionQueue { size: 16 }.into(),
        })
        .expect("post create cmpl queue msg");

    let mut cmpl_qu: Option<Queue<Completion>> = None;
    while cmpl_qu.is_none() {
        if let Some(c) = recv_qu.poll() {
            assert_eq!(c.response_to_id, 0);
            match c.kind {
                CmplKind::NewQueue(nq) => unsafe {
                    cmpl_qu = Some(Queue::from_completion(&nq));
                },
                _ => panic!("unexpected completion: {c:?}"),
            }
        }
        yield_now();
    }

    let cmpl_qu = cmpl_qu.expect("got completion queue");

    // create a submission queue and associate it with our completion queue
    send_qu
        .post(&Command {
            id: 1,
            kind: CreateSubmissionQueue {
                size: 16,
                associated_completion_queue: cmpl_qu.id,
            }
            .into(),
        })
        .expect("post create subm queue msg");

    let mut sub_qu: Option<Queue<Command>> = None;
    while sub_qu.is_none() {
        if let Some(c) = recv_qu.poll() {
            assert_eq!(c.response_to_id, 1);
            match c.kind {
                CmplKind::NewQueue(nq) => unsafe {
                    sub_qu = Some(Queue::from_completion(&nq));
                },
                _ => panic!("unexpected completion: {c:?}"),
            }
        }
        yield_now();
    }

    let sub_qu = sub_qu.expect("got submission queue");

    // try sending a test command on the new queues
    crate::cmd_test(&sub_qu, &cmpl_qu);

    // destroy both queues
    send_qu
        .post(&Command {
            id: 2,
            kind: DestroyQueue { id: sub_qu.id }.into(),
        })
        .expect("post destroy subm queue msg");
    send_qu
        .post(&Command {
            id: 3,
            kind: DestroyQueue { id: cmpl_qu.id }.into(),
        })
        .expect("post destroy cmpl queue msg");

    // wait for destroy operations to finish
    let mut outstanding = 2;
    while outstanding > 0 {
        if let Some(c) = recv_qu.poll() {
            assert!(c.response_to_id == 2 || c.response_to_id == 3);
            match c.kind {
                CmplKind::Success => {
                    outstanding -= 1;
                }
                _ => panic!("unexpected completion: {c:?}"),
            }
        }
        yield_now();
    }
}

fn fail_to_create_submission_queue_with_bad_completion_id(
    send_qu: &Queue<Command>,
    recv_qu: &Queue<Completion>,
) {
    // create a submission queue and try to associate it with an invalid completion queue ID
    send_qu
        .post(&Command {
            id: 1,
            kind: CreateSubmissionQueue {
                size: 16,
                associated_completion_queue: QueueId::new(12345).unwrap(),
            }
            .into(),
        })
        .expect("post create subm queue msg");

    // wait for the error to come back
    loop {
        if let Some(c) = recv_qu.poll() {
            assert_eq!(c.response_to_id, 1);
            match c.kind {
                CmplKind::Err(ErrorCode::InvalidId) => return,
                _ => panic!("unexpected completion: {c:?}"),
            }
        }
        yield_now();
    }
}

fn fail_to_create_queue_with_zero_size(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    // create a submission queue and try to associate it with an invalid completion queue ID
    send_qu
        .post(&Command {
            id: 1,
            kind: CreateCompletionQueue { size: 0 }.into(),
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
