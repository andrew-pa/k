use kapi::{
    commands::{
        CloseFile, Command, CreateFile, DeleteFile, OpenFile, ReadFile, ResizeFile, WriteFile,
    },
    completions::{Completion, ErrorCode, Kind as CmplKind},
    queue::Queue,
    system_calls::yield_now,
    Buffer, BufferMut, FileHandle, FileUSize, Path,
};

use crate::{wait_for_error_response, wait_for_success, Testable};

// TODO: it might be nice to run all these tests against different file systems

pub const TESTS: &[&dyn Testable] = &[
    &open_close,
    &fail_to_open_not_existing,
    &open_read_close,
    &open_partial_read_close,
    &fail_to_close_bad_handle,
    &create_close_delete,
    &fail_to_create_existing,
    &create_write_close,
    &open_read_close_created_file,
    &open_read_past_end_close_created_file,
    &open_write_past_end_close_created_file,
    &resize_append_created_file,
    &resize_truncate_created_file,
    &delete_created_file,
    &fail_to_delete_non_existing,
    &fail_to_read_bad_handle,
    &fail_to_write_bad_handle,
    &fail_to_resize_bad_handle,
    &data_round_trip_without_close,
];

fn open_close(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(Command {
            id: 0,
            kind: OpenFile {
                path: Path::from("/volumes/root/bin/test_process"),
            }
            .into(),
        })
        .expect("send open file");

    let f = wait_for_result_value!(recv_qu, 0, CmplKind::OpenedFileHandle(r) => r);

    log::debug!("got file handle: {f:?}");
    assert!(f.size > 0);

    send_qu
        .post(Command {
            id: 1,
            kind: CloseFile { handle: f.handle }.into(),
        })
        .expect("send close file");

    wait_for_success(recv_qu, 1);
}

fn create_close_delete(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    let path = Path::from("/volumes/root/tmp-test-file");

    send_qu
        .post(Command {
            id: 0,
            kind: CreateFile {
                path: path.clone(),
                initial_size: 512,
                open_if_exists: false,
            }
            .into(),
        })
        .expect("send create file");

    let f = wait_for_result_value!(recv_qu, 0, CmplKind::OpenedFileHandle(r) => r);

    log::debug!("got file handle: {f:?}");
    assert_eq!(f.size, 512);

    send_qu
        .post(Command {
            id: 1,
            kind: CloseFile { handle: f.handle }.into(),
        })
        .expect("send close file");

    wait_for_success(recv_qu, 1);

    send_qu
        .post(Command {
            id: 2,
            kind: DeleteFile { path }.into(),
        })
        .expect("send delete file");

    wait_for_success(recv_qu, 2);
}

fn fail_to_create_existing(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(Command {
            id: 0,
            kind: CreateFile {
                path: Path::from("/volumes/root/bin/test_process"),
                initial_size: 512,
                open_if_exists: false,
            }
            .into(),
        })
        .expect("send create file");

    wait_for_error_response(recv_qu, 0, ErrorCode::AlreadyExists);
}

fn fail_to_open_not_existing(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(Command {
            id: 0,
            kind: OpenFile {
                path: Path::from("/dne"),
            }
            .into(),
        })
        .expect("send open file");

    wait_for_error_response(recv_qu, 0, ErrorCode::NotFound);
}

/// The known path of the test data file that contains [KNOWN_TEST_DATA].
const KNOWN_TEST_DATA_PATH: &str = "/volumes/root/test-data-file";
/// This is the output of `printf "%.1s" {1..64}` as generated by the `Makefile`.
const KNOWN_TEST_DATA: &[u8] = b"1234567891111111111222222222233333333334444444444555555555566666";

fn open_read_close(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(Command {
            id: 0,
            kind: OpenFile {
                path: Path::from(KNOWN_TEST_DATA_PATH),
            }
            .into(),
        })
        .expect("send open file");

    let f = wait_for_result_value!(recv_qu, 0, CmplKind::OpenedFileHandle(r) => r);

    log::debug!("got file handle: {f:?}");
    assert_eq!(f.size, KNOWN_TEST_DATA.len() as FileUSize);

    let mut data = [0u8; KNOWN_TEST_DATA.len()];
    send_qu
        .post(Command {
            id: 1,
            kind: ReadFile {
                src_handle: f.handle,
                src_offset: 0,
                dst_buffer: BufferMut::from(&mut data[..]),
            }
            .into(),
        })
        .expect("send read file");

    wait_for_success(recv_qu, 1);

    assert_eq!(data, KNOWN_TEST_DATA);

    send_qu
        .post(Command {
            id: 1,
            kind: CloseFile { handle: f.handle }.into(),
        })
        .expect("send close file");

    wait_for_success(recv_qu, 1);
}

fn open_partial_read_close(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(Command {
            id: 0,
            kind: OpenFile {
                path: Path::from(KNOWN_TEST_DATA_PATH),
            }
            .into(),
        })
        .expect("send open file");

    let f = wait_for_result_value!(recv_qu, 0, CmplKind::OpenedFileHandle(r) => r);

    log::debug!("got file handle: {f:?}");
    assert_eq!(f.size, KNOWN_TEST_DATA.len() as FileUSize);

    let mut data = [0u8; 2];

    for offset in 0..KNOWN_TEST_DATA.len() - 2 {
        send_qu
            .post(Command {
                id: 1,
                kind: ReadFile {
                    src_handle: f.handle,
                    src_offset: offset as FileUSize,
                    dst_buffer: BufferMut::from(&mut data[..]),
                }
                .into(),
            })
            .expect("send read file");

        wait_for_success(recv_qu, 1);

        assert_eq!(data, KNOWN_TEST_DATA[offset..offset + data.len()]);
    }

    send_qu
        .post(Command {
            id: 1,
            kind: CloseFile { handle: f.handle }.into(),
        })
        .expect("send close file");

    wait_for_success(recv_qu, 1);
}

const CREATED_TEST_FILE_PATH: &str = "/volumes/root/test-file";
const CREATED_TEST_DATA: &[u8] = b"hello, world!\n";

fn create_write_close(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    let path = Path::from(CREATED_TEST_FILE_PATH);

    send_qu
        .post(Command {
            id: 0,
            kind: CreateFile {
                path: path.clone(),
                initial_size: CREATED_TEST_DATA.len(),
                open_if_exists: false,
            }
            .into(),
        })
        .expect("send create file");

    let f = wait_for_result_value!(recv_qu, 0, CmplKind::OpenedFileHandle(r) => r);

    log::debug!("got file handle: {f:?}");
    assert_eq!(f.size, CREATED_TEST_DATA.len() as FileUSize);

    send_qu
        .post(Command {
            id: 1,
            kind: WriteFile {
                dst_handle: f.handle,
                dst_offset: 0,
                src_buffer: Buffer::from(CREATED_TEST_DATA),
            }
            .into(),
        })
        .expect("send write file");

    send_qu
        .post(Command {
            id: 1,
            kind: CloseFile { handle: f.handle }.into(),
        })
        .expect("send close file");

    wait_for_success(recv_qu, 1);
}

fn open_read_close_created_file(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(Command {
            id: 0,
            kind: OpenFile {
                path: Path::from(CREATED_TEST_FILE_PATH),
            }
            .into(),
        })
        .expect("send open file");

    let f = wait_for_result_value!(recv_qu, 0, CmplKind::OpenedFileHandle(r) => r);

    log::debug!("got file handle: {f:?}");
    assert_eq!(f.size, CREATED_TEST_DATA.len() as FileUSize);

    let mut data = [0u8; CREATED_TEST_DATA.len()];
    send_qu
        .post(Command {
            id: 1,
            kind: ReadFile {
                src_handle: f.handle,
                src_offset: 0,
                dst_buffer: BufferMut::from(&mut data[..]),
            }
            .into(),
        })
        .expect("send read file");

    wait_for_success(recv_qu, 1);

    assert_eq!(&data, CREATED_TEST_DATA);

    send_qu
        .post(Command {
            id: 1,
            kind: CloseFile { handle: f.handle }.into(),
        })
        .expect("send close file");

    wait_for_success(recv_qu, 1);
}

fn open_read_past_end_close_created_file(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(Command {
            id: 0,
            kind: OpenFile {
                path: Path::from(CREATED_TEST_FILE_PATH),
            }
            .into(),
        })
        .expect("send open file");

    let f = wait_for_result_value!(recv_qu, 0, CmplKind::OpenedFileHandle(r) => r);

    log::debug!("got file handle: {f:?}");
    assert_eq!(f.size, CREATED_TEST_DATA.len() as FileUSize);

    let mut data = [0u8; 4];

    send_qu
        .post(Command {
            id: 1,
            kind: ReadFile {
                src_handle: f.handle,
                src_offset: CREATED_TEST_DATA.len() as FileUSize,
                dst_buffer: BufferMut::from(&mut data[..]),
            }
            .into(),
        })
        .expect("send read file");

    wait_for_error_response(recv_qu, 1, ErrorCode::OutOfBounds);

    send_qu
        .post(Command {
            id: 1,
            kind: CloseFile { handle: f.handle }.into(),
        })
        .expect("send close file");

    wait_for_success(recv_qu, 1);
}

fn open_write_past_end_close_created_file(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(Command {
            id: 0,
            kind: OpenFile {
                path: Path::from(CREATED_TEST_FILE_PATH),
            }
            .into(),
        })
        .expect("send open file");

    let f = wait_for_result_value!(recv_qu, 0, CmplKind::OpenedFileHandle(r) => r);

    log::debug!("got file handle: {f:?}");
    assert_eq!(f.size, CREATED_TEST_DATA.len() as FileUSize);

    let data = [0u8; 4];

    send_qu
        .post(Command {
            id: 1,
            kind: WriteFile {
                dst_handle: f.handle,
                dst_offset: CREATED_TEST_DATA.len() as FileUSize,
                src_buffer: Buffer::from(&data[..]),
            }
            .into(),
        })
        .expect("send read file");

    wait_for_error_response(recv_qu, 1, ErrorCode::OutOfBounds);

    send_qu
        .post(Command {
            id: 1,
            kind: CloseFile { handle: f.handle }.into(),
        })
        .expect("send close file");

    wait_for_success(recv_qu, 1);
}

fn resize_truncate_created_file(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    const NEW_SIZE: usize = CREATED_TEST_DATA.len() / 2;

    send_qu
        .post(Command {
            id: 0,
            kind: OpenFile {
                path: Path::from(CREATED_TEST_FILE_PATH),
            }
            .into(),
        })
        .expect("send open file");

    let f = wait_for_result_value!(recv_qu, 0, CmplKind::OpenedFileHandle(r) => r);

    log::debug!("got file handle: {f:?}");
    assert_eq!(f.size, CREATED_TEST_DATA.len() as FileUSize);

    send_qu
        .post(Command {
            id: 1,
            kind: ResizeFile {
                handle: f.handle,
                new_size: NEW_SIZE as FileUSize,
            }
            .into(),
        })
        .expect("send resize");

    wait_for_success(recv_qu, 1);

    // Validate the remaining data
    let mut data = [0u8; NEW_SIZE];
    send_qu
        .post(Command {
            id: 2,
            kind: ReadFile {
                src_handle: f.handle,
                src_offset: 0,
                dst_buffer: BufferMut::from(&mut data[..]),
            }
            .into(),
        })
        .expect("send read file");

    wait_for_success(recv_qu, 2);
    assert_eq!(&data, &CREATED_TEST_DATA[..NEW_SIZE]);

    // Validate that reads past the new end fail
    send_qu
        .post(Command {
            id: 3,
            kind: ReadFile {
                src_handle: f.handle,
                src_offset: NEW_SIZE as FileUSize,
                dst_buffer: BufferMut::from(&mut [0u8; 1][..]),
            }
            .into(),
        })
        .expect("send read file");

    wait_for_error_response(recv_qu, 3, ErrorCode::OutOfBounds);

    send_qu
        .post(Command {
            id: 4,
            kind: CloseFile { handle: f.handle }.into(),
        })
        .expect("send close file");

    wait_for_success(recv_qu, 4);

    // Reopen the file and check the size
    send_qu
        .post(Command {
            id: 5,
            kind: OpenFile {
                path: Path::from(CREATED_TEST_FILE_PATH),
            }
            .into(),
        })
        .expect("send open file");

    let f = wait_for_result_value!(recv_qu, 5, CmplKind::OpenedFileHandle(r) => r);

    log::debug!("got file handle: {f:?}");
    assert_eq!(f.size, NEW_SIZE as FileUSize);

    send_qu
        .post(Command {
            id: 6,
            kind: CloseFile { handle: f.handle }.into(),
        })
        .expect("send close file");

    wait_for_success(recv_qu, 6);

    send_qu
        .post(Command {
            id: 3,
            kind: CloseFile { handle: f.handle }.into(),
        })
        .expect("send close file");

    wait_for_success(recv_qu, 3);
}

fn resize_append_created_file(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    const NEW_SIZE: usize = CREATED_TEST_DATA.len() * 2;

    send_qu
        .post(Command {
            id: 0,
            kind: OpenFile {
                path: Path::from(CREATED_TEST_FILE_PATH),
            }
            .into(),
        })
        .expect("send open file");

    let f = wait_for_result_value!(recv_qu, 0, CmplKind::OpenedFileHandle(r) => r);

    log::debug!("got file handle: {f:?}");
    assert_eq!(f.size, CREATED_TEST_DATA.len() as FileUSize);

    send_qu
        .post(Command {
            id: 1,
            kind: ResizeFile {
                handle: f.handle,
                new_size: NEW_SIZE as FileUSize,
            }
            .into(),
        })
        .expect("send resize");

    wait_for_success(recv_qu, 1);

    // Validate the original data
    let mut data = [0u8; CREATED_TEST_DATA.len()];
    send_qu
        .post(Command {
            id: 2,
            kind: ReadFile {
                src_handle: f.handle,
                src_offset: 0,
                dst_buffer: BufferMut::from(&mut data[..]),
            }
            .into(),
        })
        .expect("send read file");

    wait_for_success(recv_qu, 2);
    assert_eq!(&data, &CREATED_TEST_DATA);

    // Validate that the new bytes are zero
    let mut new_data = [0u8; NEW_SIZE - CREATED_TEST_DATA.len()];
    send_qu
        .post(Command {
            id: 3,
            kind: ReadFile {
                src_handle: f.handle,
                src_offset: CREATED_TEST_DATA.len() as FileUSize,
                dst_buffer: BufferMut::from(&mut new_data[..]),
            }
            .into(),
        })
        .expect("send read file");

    wait_for_success(recv_qu, 3);
    assert!(new_data.iter().all(|&byte| byte == 0));

    send_qu
        .post(Command {
            id: 4,
            kind: CloseFile { handle: f.handle }.into(),
        })
        .expect("send close file");

    wait_for_success(recv_qu, 4);

    // Reopen the file and check the size
    send_qu
        .post(Command {
            id: 5,
            kind: OpenFile {
                path: Path::from(CREATED_TEST_FILE_PATH),
            }
            .into(),
        })
        .expect("send open file");

    let f = wait_for_result_value!(recv_qu, 5, CmplKind::OpenedFileHandle(r) => r);

    log::debug!("got file handle: {f:?}");
    assert_eq!(f.size, NEW_SIZE as FileUSize);

    send_qu
        .post(Command {
            id: 6,
            kind: CloseFile { handle: f.handle }.into(),
        })
        .expect("send close file");

    wait_for_success(recv_qu, 6);
}

fn delete_created_file(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(Command {
            id: 2,
            kind: DeleteFile {
                path: Path::from(CREATED_TEST_FILE_PATH),
            }
            .into(),
        })
        .expect("send delete file");

    wait_for_success(recv_qu, 2);

    //make sure it is gone

    send_qu
        .post(Command {
            id: 3,
            kind: OpenFile {
                path: Path::from(CREATED_TEST_FILE_PATH),
            }
            .into(),
        })
        .expect("send open file");

    wait_for_error_response(recv_qu, 3, ErrorCode::NotFound);
}

fn fail_to_delete_non_existing(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(Command {
            id: 0,
            kind: DeleteFile {
                path: Path::from("/dne"),
            }
            .into(),
        })
        .expect("send delete file");

    wait_for_error_response(recv_qu, 0, ErrorCode::NotFound);
}

fn fail_to_read_bad_handle(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    let mut data = [0u8; 1];
    send_qu
        .post(Command {
            id: 0,
            kind: ReadFile {
                src_handle: FileHandle::new(12345678).unwrap(),
                src_offset: 0,
                dst_buffer: BufferMut::from(&mut data[..]),
            }
            .into(),
        })
        .expect("send read file");

    wait_for_error_response(recv_qu, 0, ErrorCode::InvalidId);
}

fn fail_to_write_bad_handle(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    let data = [0u8; 1];
    send_qu
        .post(Command {
            id: 0,
            kind: WriteFile {
                dst_handle: FileHandle::new(12345678).unwrap(),
                dst_offset: 0,
                src_buffer: Buffer::from(&data[..]),
            }
            .into(),
        })
        .expect("send write file");

    wait_for_error_response(recv_qu, 0, ErrorCode::InvalidId);
}

fn fail_to_resize_bad_handle(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(Command {
            id: 0,
            kind: ResizeFile {
                handle: FileHandle::new(12345678).unwrap(),
                new_size: 1000,
            }
            .into(),
        })
        .expect("send resize file");

    wait_for_error_response(recv_qu, 0, ErrorCode::InvalidId);
}

fn fail_to_close_bad_handle(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    send_qu
        .post(Command {
            id: 0,
            kind: CloseFile {
                handle: FileHandle::new(12345678).unwrap(),
            }
            .into(),
        })
        .expect("send close file");

    wait_for_error_response(recv_qu, 0, ErrorCode::InvalidId);
}

fn data_round_trip_without_close(send_qu: &Queue<Command>, recv_qu: &Queue<Completion>) {
    let path = Path::from("/volumes/root/tmp-test-file");

    send_qu
        .post(Command {
            id: 0,
            kind: CreateFile {
                path: path.clone(),
                initial_size: 16,
                open_if_exists: false,
            }
            .into(),
        })
        .expect("send create file");

    let f = wait_for_result_value!(recv_qu, 0, CmplKind::OpenedFileHandle(r) => r);

    log::debug!("got file handle: {f:?}");
    assert_eq!(f.size, 16);

    let data = b"hello, world!!!!";

    send_qu
        .post(Command {
            id: 1,
            kind: WriteFile {
                dst_handle: f.handle,
                dst_offset: 0,
                src_buffer: Buffer::from(data.as_slice()),
            }
            .into(),
        })
        .expect("send write");

    wait_for_success(recv_qu, 1);

    let mut dst_data = [0u8; 16];

    send_qu
        .post(Command {
            id: 2,
            kind: ReadFile {
                src_handle: f.handle,
                src_offset: 0,
                dst_buffer: BufferMut::from(&mut dst_data[..]),
            }
            .into(),
        })
        .expect("send write");

    wait_for_success(recv_qu, 2);

    assert_eq!(data, &dst_data);

    send_qu
        .post(Command {
            id: 3,
            kind: CloseFile { handle: f.handle }.into(),
        })
        .expect("send close file");

    wait_for_success(recv_qu, 1);

    send_qu
        .post(Command {
            id: 4,
            kind: DeleteFile { path }.into(),
        })
        .expect("send delete file");

    wait_for_success(recv_qu, 2);
}
