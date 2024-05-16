//! Handlers and dispatch for user-space commands.
use core::ptr::NonNull;

use alloc::sync::Weak;
use futures::FutureExt;
use kapi::{
    commands::{self as cmds, Kind as CmdKind},
    completions::{self as cmpl, ErrorCode, Kind as CmplKind},
};
use snafu::ensure;

use crate::{
    error::{self, Error, InnerSnafu, MemorySnafu, Utf8Snafu},
    process::*,
    registry::{registry, PathBuf},
};

use self::memory::paging::kernel_table;

macro_rules! command_dispatch {
    {
        $kind:expr,
        $(
            $cmd:pat => $res:expr
        ),*
    } => {
        match $kind {
            $($cmd => $res.map(Into::into),)*
            kind => Err(Error::Misc {
                reason: alloc::format!(
                            "received unknown command: {:?}",
                            core::mem::discriminant(kind)
                        ),
                code: Some(ErrorCode::UnknownCommand),
            }),
        }
    };
}

/// Implementation of user-space commands.
impl Process {
    async fn create_queue<T>(
        &self,
        num_elements: usize,
    ) -> Result<(UserQueue<T>, cmpl::NewQueue), Error> {
        if num_elements == 0 {
            return Err(Error::Misc {
                reason: "queue must have at least a capacity of 1, got 0".into(),
                code: Some(ErrorCode::InvalidSize),
            });
        }

        let id = self.next_queue_id()?;

        let page_count = kapi::queue::queue_size_in_bytes::<T>(num_elements).div_ceil(PAGE_SIZE);

        let (user_base_address, kernel_base_address) = self.alloc_memory(page_count, true).await?;

        let kernel_base_address = kernel_base_address.unwrap();

        log::trace!("creating queue user:{user_base_address}, kernel:{kernel_base_address}");

        let queue = unsafe {
            Queue::new(
                id,
                NonNull::new(kernel_base_address.as_ptr())
                    .expect("allocated memory for queue is non-null"),
                num_elements,
            )
        };

        unsafe {
            queue.initialize();
        }

        let qu = UserQueue {
            queue,
            user_base_address,
        };

        Ok((
            qu,
            cmpl::NewQueue {
                id,
                start: user_base_address.0,
            },
        ))
    }

    pub async fn create_completion_queue(
        &self,
        info: &cmds::CreateCompletionQueue,
    ) -> Result<cmpl::NewQueue, Error> {
        let (oq, nq) = self.create_queue::<Completion>(info.size).await?;
        self.recv_queues.push(Arc::new(oq));
        Ok(nq)
    }

    pub async fn create_submission_queue(
        &self,
        info: &cmds::CreateSubmissionQueue,
    ) -> Result<cmpl::NewQueue, Error> {
        let rq = self
            .recv_queues
            .iter()
            .find(|q| q.id == info.associated_completion_queue)
            .cloned()
            .with_context(|| error::MiscSnafu {
                reason: alloc::format!(
                    "completion queue {} not found",
                    info.associated_completion_queue
                ),
                code: Some(ErrorCode::InvalidId),
            })?;
        let (oq, nq) = self.create_queue::<Command>(info.size).await?;
        self.send_queues.push((Arc::new(oq), rq));
        Ok(nq)
    }

    fn destroy_queue(&self, info: &cmds::DestroyQueue) -> Result<cmpl::Success, Error> {
        if self.send_queues.iter().any(|(_, q)| q.id == info.id) {
            Err(Error::Misc {
                reason: "completion queue still in use".into(),
                code: Some(ErrorCode::InUse),
            })
        } else {
            let q = self
                .recv_queues
                .remove_cloned(|q| q.id == info.id)
                .map(|q| q.memory_region())
                .or(self
                    .send_queues
                    .remove_cloned(|(q, _)| q.id == info.id)
                    .map(|(q, _)| q.memory_region()));
            match q {
                Some((user_addr, kernel_addr, page_count)) => {
                    self.free_memory(user_addr, page_count);
                    kernel_table().unmap_range(kernel_addr, page_count);
                    Ok(cmpl::Success)
                }
                None => Err(Error::Misc {
                    reason: "queue ID unknown".into(),
                    code: Some(ErrorCode::InvalidId),
                }),
            }
        }
    }

    async fn spawn_thread(
        self: &Arc<Process>,
        info: &cmds::SpawnThread,
        cmd_id: u16,
        recv_qu: Weak<UserQueue<Completion>>,
    ) -> Result<cmpl::NewThread, Error> {
        ensure!(
            info.stack_size > 0,
            error::MiscSnafu {
                code: Some(ErrorCode::InvalidSize),
                reason: "thread stack must have non-zero size"
            }
        );

        let stack_page_count = info.stack_size.div_ceil(PAGE_SIZE);

        // TODO: free this memory when the threads exit!
        let initial_stack_pointer = self
            .alloc_memory(stack_page_count, false)
            .await
            .context(InnerSnafu {
                reason: "allocate thread stack",
            })?
            .0;

        let tid = thread::next_thread_id();
        let thread = Arc::new(Thread::user_thread(
            self.clone(),
            tid,
            VirtualAddress(info.entry_point as usize),
            initial_stack_pointer,
            stack_page_count,
            ThreadPriority::Normal,
            Registers::from_args(&[info.user_data as usize]),
        ));

        if info.send_completion_on_exit {
            crate::tasks::spawn(thread.exit_code().map(move |ec| {
                if let Some(rq) = recv_qu.upgrade() {
                    // TODO: deal with queue overflow
                    let _ = rq.post(Completion {
                        response_to_id: cmd_id,
                        kind: ec.into(),
                    });
                }
            }));
        }

        self.threads.lock().await.push(thread.clone());
        thread::spawn_thread(thread);

        Ok(cmpl::NewThread { id: tid })
    }

    async fn watch_thread(
        self: &Arc<Process>,
        info: &cmds::WatchThread,
    ) -> Result<cmpl::ThreadExit, Error> {
        let thread = thread_for_id(info.thread_id).context(error::MiscSnafu {
            reason: "find thread to watch",
            code: Some(ErrorCode::InvalidId),
        })?;

        Ok(thread.exit_code().await)
    }

    /// Read a path from user-space into the kernel.
    fn read_user_path(&self, path: &kapi::Path) -> Result<PathBuf, Error> {
        let mut path_buf = alloc::vec![0; path.len];
        self.page_tables
            .mapped_copy_from(path.text.into(), &mut path_buf)
            .context(error::MemorySnafu {
                reason: "invalid path slice",
            })?;
        PathBuf::from_bytes(path_buf).with_context(|_| Utf8Snafu {
            reason: alloc::format!("user path: {:?}", path),
        })
    }

    async fn spawn_child(
        self: &Arc<Process>,
        info: &cmds::SpawnProcess,
        cmd_id: u16,
        recv_qu: Weak<UserQueue<Completion>>,
    ) -> Result<cmpl::NewProcess, Error> {
        // convert info.binary_path into a kernel Path
        let binary_path = self.read_user_path(&info.binary_path)?;

        let params_buf = (info.parameters.len > 0)
            .then(|| {
                let mut buf = PhysicalBuffer::alloc_zeroed(
                    info.parameters.len.div_ceil(PAGE_SIZE),
                    &PageTableEntryOptions {
                        read_only: false,
                        el0_access: true,
                    },
                )
                .context(MemorySnafu {
                    reason: "allocate buffer for process parameters",
                })?;
                self.page_tables
                    .mapped_copy_from(
                        info.parameters.data.into(),
                        &mut buf.as_bytes_mut()[0..info.parameters.len],
                    )
                    .context(MemorySnafu {
                        reason: "copy process parameters into new process",
                    })?;
                log::trace!("created parameter buffer {:?} -> {buf:?}", info.parameters);
                Ok((buf, info.parameters.len))
            })
            .transpose()?;

        // spawn process
        let proc = spawn_process(binary_path, params_buf, |proc: Arc<Process>| {
            // watch for exit if requested
            if info.send_completion_on_main_thread_exit {
                crate::tasks::spawn(proc.exit_code().map(move |ec| {
                    if let Some(rq) = recv_qu.upgrade() {
                        // TODO: deal with queue overflow
                        let _ = rq.post(Completion {
                            response_to_id: cmd_id,
                            kind: ec.into(),
                        });
                    }
                }));
            }
        })
        .await?;

        Ok(cmpl::NewProcess { id: proc.id })
    }

    async fn watch_process(
        self: &Arc<Process>,
        info: &cmds::WatchProcess,
    ) -> Result<cmpl::ThreadExit, Error> {
        let proc = process_for_id(info.process_id).context(error::MiscSnafu {
            reason: "find process to watch",
            code: Some(ErrorCode::InvalidId),
        })?;

        Ok(proc.exit_code().await)
    }

    async fn open_file(
        self: &Arc<Process>,
        info: &cmds::OpenFile,
    ) -> Result<cmpl::OpenedFileHandle, Error> {
        let path = self.read_user_path(&info.path)?;
        let handle = FileHandle::new(
            self.next_handle
                .fetch_add(1, core::sync::atomic::Ordering::AcqRel),
        )
        .expect("process next_handle counter contains valid handle");
        let file = registry().open_file(&path).await?;
        let size = file.len();
        self.open_files.push(Arc::new(OpenFile {
            handle,
            file: crate::tasks::locks::Mutex::new(file),
        }));
        Ok(cmpl::OpenedFileHandle { handle, size })
    }

    async fn close_file(
        self: &Arc<Process>,
        info: &cmds::CloseFile,
    ) -> Result<cmpl::Success, Error> {
        if self.open_files.remove(|f| f.handle == info.handle) {
            Ok(cmpl::Success)
        } else {
            Err(Error::Misc {
                reason: "close on unknown handle".into(),
                code: Some(ErrorCode::InvalidId),
            })
        }
    }

    /// Execute a single command on the process, returning the resulting completion.
    pub async fn dispatch_command(
        self: &Arc<Process>,
        cmd: Command,
        recv_qu: Weak<UserQueue<Completion>>,
    ) -> Completion {
        let res = command_dispatch! {
            &cmd.kind,
            CmdKind::Test(cmds::Test { arg }) => Ok(cmpl::Test {
                arg: *arg,
                pid: self.id,
            }),
            CmdKind::CreateCompletionQueue(info) => self.create_completion_queue(info).await,
            CmdKind::CreateSubmissionQueue(info) => self.create_submission_queue(info).await,
            CmdKind::DestroyQueue(info) => self.destroy_queue(info),
            CmdKind::SpawnThread(info) => self.spawn_thread(info, cmd.id, recv_qu).await,
            CmdKind::WatchThread(info) => self.watch_thread(info).await,
            CmdKind::SpawnProcess(info) => self.spawn_child(info, cmd.id, recv_qu).await,
            CmdKind::WatchProcess(info) => self.watch_process(info).await,
            CmdKind::KillProcess(info) => kill_process(info).await,
            CmdKind::OpenFile(info) => self.open_file(info).await,
            CmdKind::CloseFile(info) => self.close_file(info).await
        };

        let kind = res.unwrap_or_else(|e| {
            log::error!(
                "[pid {}] error occurred processing {cmd:?}:\n{}",
                self.id,
                snafu::Report::from_error(&e)
            );
            CmplKind::Err(e.as_code())
        });

        Completion {
            response_to_id: cmd.id,
            kind,
        }
    }
}

async fn kill_process(info: &cmds::KillProcess) -> Result<cmpl::Success, Error> {
    let process = process_for_id(info.process_id).context(error::MiscSnafu {
        reason: "find process to kill",
        code: Some(ErrorCode::InvalidId),
    })?;

    let threads = {
        let t = process.threads.lock().await;
        t.clone()
    };

    for thread in threads {
        thread.exit(cmpl::ThreadExit::Killed);
    }

    Ok(cmpl::Success)
}
