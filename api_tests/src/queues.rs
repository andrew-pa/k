use kapi::{
    commands::{Command, CreateCompletionQueue, CreateSubmissionQueue},
    completions::{Completion, Kind as CmplKind},
    queue::Queue,
    system_calls::yield_now,
};

use crate::Testable;

pub const TESTS: &[&dyn Testable] = &[&basic_create_destroy];

pub fn basic_create_destroy(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
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
                _ => panic!(),
            }
        }
        yield_now();
    }

    let cmpl_qu = cmpl_qu.expect("got completion queue");

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
                _ => panic!(),
            }
        }
        yield_now();
    }

    let sub_qu = sub_qu.expect("got submission queue");

    // try sending a test command on the new queues
    crate::cmd_test(&sub_qu, &cmpl_qu);
}
