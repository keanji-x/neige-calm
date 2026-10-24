use super::*;
use std::{fs::File, os::unix::fs::PermissionsExt, task::Poll, time::Duration};
use tokio::io::AsyncWriteExt;

struct Fixture {
    executor: Arc<Executor>,
    request: PrepareExecution,
    process: Arc<process::Process>,
    reports: Option<File>,
    helper: tokio::net::UnixStream,
    target: Option<tokio::net::UnixStream>,
    _temp: tempfile::TempDir,
}
impl Fixture {
    async fn new(mode: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let executor = Executor::open(ExecutorConfig {
            journal_directory: temp.path().join("journal"),
            helper_program: std::env::current_exe().unwrap(),
            kernel_client_secret: CapabilityToken::fresh().unwrap(),
        })
        .unwrap();
        let (mut request, held) = protocol_tests::private(&executor, temp.path());
        let helper_path = temp.path().join("helper.sock");
        let target_path = temp.path().join("target.sock");
        let helper_listener = tokio::net::UnixListener::bind(&helper_path).unwrap();
        let target_listener = tokio::net::UnixListener::bind(&target_path).unwrap();
        request.specification.program = "/usr/bin/python3".into();
        request.specification.args = vec![
            "-c".into(),
            format!(
                "import os,socket; os.close(0); s=socket.socket(socket.AF_UNIX); s.connect({:?}); s.recv(1)",
                target_path.to_str().unwrap()
            ),
        ];
        if mode == "failure" {
            request.specification.program = "/no-such-stdin-handoff-target".into();
        }
        protocol_tests::prepare(&executor, &request, &held).await;
        let shim = temp.path().join("helper-entry");
        let body = temp.path().join("helper-body");
        let executable = std::env::current_exe()
            .unwrap()
            .to_str()
            .unwrap()
            .replace('\'', "'\"'\"'");
        std::fs::write(&shim, format!("#!/bin/sh {}\n", body.display())).unwrap();
        std::fs::write(
            &body,
            format!(
                concat!(
                    "E1A_CONFIG_FD=$3 E1A_REPORT_FD=$4 ",
                    "E1A_HANDOFF_SOCKET='{}' E1A_HANDOFF_MODE='{}' ",
                    "exec '{}' --ignored --exact {} --nocapture\n",
                ),
                helper_path.display(),
                mode,
                executable,
                "execution::stdin_handoff_tests::helper_entry"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700)).unwrap();
        let output = output::OutputLog::open(&temp.path().join("output"), 1024).unwrap();
        let context = scope_helper::HelperContext {
            execution_id: request.identity.execution_id.clone(),
            executor_boot: executor.boot.clone(),
            nonce: CapabilityToken::fresh().unwrap(),
            mechanism: executor.mechanism.clone(),
        };
        let (process, native, reports) = process::spawn(
            &File::open(shim).unwrap(),
            &File::open(temp.path()).unwrap(),
            &request.specification,
            context,
            output.clone(),
        )
        .unwrap();
        let receipt = ExecutionReceipt {
            identity: request.identity.clone(),
            slot_version: 1,
            process: native,
            scope_mechanism: executor.mechanism.clone(),
        };
        executor
            .persist(Journal {
                version: journal::JOURNAL_VERSION,
                host_boot_id: executor.host_boot_id.clone(),
                output_end: OutputEnd::Open,
                request: request.clone(),
                phase: Phase::Spawned(receipt),
                interactive_revoked: false,
                pending_persistence: false,
            })
            .unwrap();
        let id = request.identity.execution_id.clone();
        executor
            .processes
            .lock()
            .unwrap()
            .insert(id.clone(), process.clone());
        executor.outputs.lock().unwrap().insert(id, output);
        let (helper, _) = helper_listener.accept().await.unwrap();
        // The real target has closed fd0, while the helper is paused before releasing its reader.
        let target = if mode == "failure" {
            None
        } else {
            Some(target_listener.accept().await.unwrap().0)
        };
        Self {
            executor,
            request,
            process,
            reports: Some(reports),
            helper,
            target,
            _temp: temp,
        }
    }
    fn monitor(&mut self) {
        self.executor.monitor(
            self.request.identity.execution_id.clone(),
            self.reports.take().unwrap(),
            self.process.clone(),
        );
    }
    fn control(&self, action: ProcessAction, purpose: ExecutionControlPurpose) -> ExecutionControl {
        ExecutionControl {
            capability: ExecutionCapability {
                identity: self.request.identity.clone(),
                slot_version: 1,
                token: match purpose {
                    ExecutionControlPurpose::Interactive => {
                        self.request.control_tokens.interactive.clone()
                    }
                    ExecutionControlPurpose::Lifecycle => {
                        self.request.control_tokens.lifecycle.clone()
                    }
                    ExecutionControlPurpose::Observe => self.request.control_tokens.observe.clone(),
                },
                purpose,
            },
            action,
        }
    }
}

#[tokio::test]
async fn stdin_handoff_waits_for_reader_release_then_reports_broken_pipe() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut fx = Fixture::new("normal").await;
        let control = fx.control(
            ProcessAction::WriteStdin {
                bytes: b"must fail".to_vec(),
            },
            ExecutionControlPurpose::Interactive,
        );
        fx.monitor();
        let write = fx.executor.control(control);
        tokio::pin!(write);
        let early = std::future::poll_fn(|cx| {
            Poll::Ready(match write.as_mut().poll(cx) {
                Poll::Ready(result) => Some(result),
                Poll::Pending => None,
            })
        })
        .await;
        fx.helper.write_all(b"release").await.unwrap();
        let result = match early {
            Some(result) => result,
            None => write.await,
        };
        fx.target
            .as_mut()
            .unwrap()
            .write_all(b"exit")
            .await
            .unwrap();
        assert!(
            matches!(&result, Err(error) if error.to_string().contains("Broken pipe")),
            "{result:?}"
        );
        fx.executor.graceful_shutdown().await;
    })
    .await
    .unwrap();
}

async fn interrupted(mode: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut fx = Fixture::new(mode).await;
        let write_control = fx.control(
            ProcessAction::WriteStdin {
                bytes: b"must fail".to_vec(),
            },
            ExecutionControlPurpose::Interactive,
        );
        let stop = fx.control(ProcessAction::Stop, ExecutionControlPurpose::Lifecycle);
        fx.monitor();
        let write = fx.executor.control(write_control);
        tokio::pin!(write);
        std::future::poll_fn(|cx| {
            assert!(
                write.as_mut().poll(cx).is_pending(),
                "write crossed an unreleased handoff"
            );
            Poll::Ready(())
        })
        .await;
        match mode {
            "stop" => {
                fx.executor.control(stop).await.unwrap();
            }
            "revoked" => {
                fx.executor
                    .revoke(protocol_tests::consume(&fx.request))
                    .await
                    .unwrap();
            }
            "eof" => {
                fx.target
                    .as_mut()
                    .unwrap()
                    .write_all(b"exit")
                    .await
                    .unwrap();
            }
            "failure" => {}
            _ => unreachable!(),
        }
        // Stop must wake the pending write even while the helper remains at the barrier.
        if mode == "stop" {
            assert!(write.as_mut().await.is_err());
        }
        fx.helper.write_all(b"release").await.unwrap();
        if mode != "stop" {
            let error = write.await.unwrap_err();
            if mode == "revoked" {
                assert!(
                    error.to_string().contains("interaction explicitly revoked"),
                    "{error}"
                );
            }
        }
        if let Some(target) = fx.target.as_mut()
            && mode != "eof"
            && mode != "stop"
        {
            target.write_all(b"exit").await.unwrap();
        }
        fx.executor.graceful_shutdown().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn stdin_handoff_spawn_failure_unblocks_write() {
    interrupted("failure").await;
}
#[tokio::test]
async fn stdin_handoff_report_eof_unblocks_write() {
    interrupted("eof").await;
}
#[tokio::test]
async fn stdin_handoff_stop_unblocks_write_before_release() {
    interrupted("stop").await;
}
#[tokio::test]
async fn stdin_handoff_revalidates_revocation_after_release() {
    interrupted("revoked").await;
}

#[test]
#[ignore = "isolated stdin handoff helper entry"]
fn helper_entry() {
    use std::io::Read;
    let config = std::env::var("E1A_CONFIG_FD").unwrap().parse().unwrap();
    let report = std::env::var("E1A_REPORT_FD").unwrap().parse().unwrap();
    let mode = std::env::var("E1A_HANDOFF_MODE").unwrap();
    let barrier = || {
        let mut barrier =
            std::os::unix::net::UnixStream::connect(std::env::var("E1A_HANDOFF_SOCKET").unwrap())
                .unwrap();
        barrier.read_exact(&mut [0]).unwrap();
    };
    scope_helper::run_with_hooks(
        config,
        report,
        |command| {
            if mode == "failure" {
                use std::os::{fd::AsRawFd, unix::process::CommandExt};
                let socket = std::os::unix::net::UnixStream::connect(
                    std::env::var("E1A_HANDOFF_SOCKET").unwrap(),
                )
                .unwrap();
                // SAFETY: the isolated child only reads its owned barrier FD before exec.
                unsafe {
                    command.pre_exec(move || {
                        let mut byte = [0u8];
                        let count = libc::read(socket.as_raw_fd(), byte.as_mut_ptr().cast(), 1);
                        if count == 1 {
                            Ok(())
                        } else {
                            Err(std::io::Error::last_os_error())
                        }
                    });
                }
            }
        },
        || {
            if mode != "failure" {
                barrier();
            }
            if mode == "eof" {
                std::process::exit(0);
            }
        },
    )
    .unwrap();
}
