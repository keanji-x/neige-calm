//! FD lifetime through the registered production executor, with history retained.
use super::execution_support::*;
use calm_session::control::{IoMode, execution::*};
use std::time::Duration;

#[cfg(target_os = "linux")]
fn fd_count(pid: u32) -> usize {
    std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .count()
}
#[cfg(target_os = "macos")]
fn fd_count(pid: u32) -> usize {
    // libproc reports bytes; leave spare capacity and reject truncated snapshots.
    let mut fds = vec![unsafe { std::mem::zeroed::<libc::proc_fdinfo>() }; 4096];
    let bytes = std::mem::size_of_val(fds.as_slice());
    let used = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDLISTFDS,
            0,
            fds.as_mut_ptr().cast(),
            bytes as i32,
        )
    };
    assert!(used > 0 && (used as usize) < bytes);
    used as usize / std::mem::size_of::<libc::proc_fdinfo>()
}
async fn settled(client: &mut RegisteredExecutorConnection, request: &PrepareExecution) -> u64 {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let ExecutionReply::Inspected {
            state,
            output_end: OutputEnd::Complete { final_cursor },
            ..
        } = client
            .request(ExecutionRequest::Inspect(
                request.identity.execution_id.clone(),
            ))
            .await
            .unwrap()
            && state.permits_handoff()
        {
            #[cfg(target_os = "macos")]
            assert_eq!(
                state,
                ExecutionState::OriginalGroupEmpty { status: Some(0) }
            );
            #[cfg(target_os = "linux")]
            assert_eq!(state, ExecutionState::Drained { status: Some(0) });
            return final_cursor;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "execution never completed output"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
async fn bounded_fds(pid: u32, baseline: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let count = fd_count(pid);
        if count <= baseline + 2 {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "retained history grew native/log FDs: baseline={baseline}, now={count}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
fn restart(h: &mut Harness) {
    #[cfg(target_os = "macos")]
    h.child.close();
    #[cfg(target_os = "linux")]
    {
        // This unreaped test Child pins the PID until wait.
        assert_eq!(unsafe { libc::kill(h.child.id() as i32, libc::SIGTERM) }, 0);
        assert!(h.child.wait().unwrap().success());
    }
    h.child = Harness::spawn(h.temp.path(), &h.socket);
}
async fn retained_history(mode: IoMode, reopen: bool) {
    let mut h = Harness::start().await;
    let mut client = h.connect().await;
    // Flush the startup handshake connection before sampling the baseline.
    tokio::time::sleep(Duration::from_millis(30)).await;
    let baseline = fd_count(h.child.id());
    let root = h.root("cwd", false);
    let auth = authority();
    let (scope, registration) = external(root, &auth);
    let peer = prepare(
        &client,
        scope.clone(),
        &registration,
        auth.clone(),
        "live-peer",
        IoMode::Pipe,
        "printf ready; read value; printf '%s' \"$value\"",
    );
    let peer_receipt = launch(&mut client, &peer).await;
    wait_for(&mut client, &peer, &peer_receipt, |_, output| {
        output == "ready"
    })
    .await;
    let mut history = Vec::new();
    for i in 0..8 {
        let request = prepare(
            &client,
            scope.clone(),
            &registration,
            auth.clone(),
            &format!("history-{i}"),
            mode.clone(),
            "printf history-retained",
        );
        let receipt = launch(&mut client, &request).await;
        assert_eq!(settled(&mut client, &request).await, 16);
        history.push((request, receipt));
    }
    assert!(matches!(
        client
            .request(ExecutionRequest::Retire {
                execution_id: peer.identity.execution_id.clone(),
                lifecycle: peer.control_tokens.lifecycle.clone(),
            })
            .await
            .unwrap(),
        ExecutionReply::Busy(BusyReason::OutputOpen)
    ));
    assert!(matches!(
        client
            .request(ExecutionRequest::Control(ExecutionControl {
                capability: capability(&peer, &peer_receipt, ExecutionControlPurpose::Interactive),
                action: ProcessAction::WriteStdin {
                    bytes: b"alive\n".to_vec()
                },
            }))
            .await
            .unwrap(),
        ExecutionReply::StdinProgress { accepted_bytes: 6 }
    ));
    settled(&mut client, &peer).await;
    assert_eq!(
        observe(&mut client, &peer, &peer_receipt).await.1,
        "readyalive"
    );
    if reopen {
        drop(client);
        restart(&mut h);
        client = h.connect().await;
    }
    // No Retire before this assertion: history must not cost a resident FD per execution.
    bounded_fds(h.child.id(), baseline).await;
    for (request, receipt) in history {
        assert_eq!(
            observe(&mut client, &request, &receipt).await.1,
            "history-retained"
        );
        assert!(
            matches!(client.request(ExecutionRequest::Control(ExecutionControl {
            capability: capability(&request, &receipt, ExecutionControlPurpose::Observe),
            action: ProcessAction::Observe { from_cursor: Some(8) },
        })).await.unwrap(), ExecutionReply::Observation { cursor: 16, output, output_end: OutputEnd::Complete { final_cursor: 16 }, .. } if output == b"retained")
        );
        // Bad capabilities remain refused after the native owner is gone.
        assert!(matches!(
            client
                .request(ExecutionRequest::Retire {
                    execution_id: request.identity.execution_id.clone(),
                    lifecycle: CapabilityToken::fresh().unwrap(),
                })
                .await
                .unwrap(),
            ExecutionReply::Refused { .. }
        ));
        assert!(matches!(
            client
                .request(ExecutionRequest::Retire {
                    execution_id: request.identity.execution_id.clone(),
                    lifecycle: request.control_tokens.lifecycle.clone(),
                })
                .await
                .unwrap(),
            ExecutionReply::Retired(_)
        ));
        assert!(
            !h.journal()
                .join(format!("{}.output", request.identity.execution_id.as_str()))
                .exists()
        );
    }
}
#[tokio::test]
async fn fd_resources_pipe_history_without_retire() {
    retained_history(IoMode::Pipe, false).await;
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn fd_resources_pty_history_without_retire() {
    retained_history(IoMode::Pty { cols: 80, rows: 24 }, false).await;
}
#[tokio::test]
async fn fd_resources_reopen_history_without_resident_logs() {
    retained_history(IoMode::Pipe, true).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn fd_resources_drained_output_open_keeps_native_owner() {
    let h = Harness::start().await;
    let baseline = fd_count(h.child.id());
    let (mut client, request, receipt, writer) =
        super::execution_lifecycle::escaped_output(&h, IoMode::Pipe).await;
    client
        .request(ExecutionRequest::Control(ExecutionControl {
            capability: capability(&request, &receipt, ExecutionControlPurpose::Interactive),
            action: ProcessAction::WriteStdin {
                bytes: b"done\n".to_vec(),
            },
        }))
        .await
        .unwrap();
    wait_for(&mut client, &request, &receipt, |state, output| {
        matches!(state, ExecutionState::Drained { .. }) && output.contains("tail")
    })
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    // The helper has actually been reaped, but the transported stdout still owns an open writer.
    while std::path::Path::new(&format!("/proc/{}", receipt.process.pid())).exists() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(matches!(
        client
            .request(ExecutionRequest::Inspect(
                request.identity.execution_id.clone()
            ))
            .await
            .unwrap(),
        ExecutionReply::Inspected {
            output_end: OutputEnd::Open,
            ..
        }
    ));
    let targets: Vec<_> = std::fs::read_dir(format!("/proc/{}/fd", h.child.id()))
        .unwrap()
        .filter_map(|e| std::fs::read_link(e.ok()?.path()).ok())
        .collect();
    assert!(
        targets
            .iter()
            .any(|p| p.to_string_lossy().contains("pidfd")),
        "OutputOpen discarded the native owner before output settlement"
    );
    assert!(
        targets.contains(
            &h.journal()
                .join(format!("{}.output", request.identity.execution_id.as_str()))
        )
    );
    assert!(matches!(
        client
            .request(ExecutionRequest::Retire {
                execution_id: request.identity.execution_id.clone(),
                lifecycle: request.control_tokens.lifecycle.clone(),
            })
            .await
            .unwrap(),
        ExecutionReply::Busy(BusyReason::OutputOpen)
    ));
    drop(writer);
    settled(&mut client, &request).await;
    bounded_fds(h.child.id(), baseline).await;
    assert!(
        observe(&mut client, &request, &receipt)
            .await
            .1
            .contains("readytail")
    );
    assert!(
        std::fs::read_dir(format!("/proc/{}/fd", h.child.id()))
            .unwrap()
            .filter_map(|e| std::fs::read_link(e.ok()?.path()).ok())
            .all(|p| !p.to_string_lossy().contains("pidfd")),
        "completed output retained the reaped native owner"
    );
}

#[tokio::test]
async fn fd_resources_history_reopen_validates_length_and_never_creates_missing_log() {
    use calm_proc_supervisor::execution::{Executor, ExecutorConfig};
    let h = Harness::start().await;
    let mut client = h.connect().await;
    let auth = authority();
    let (scope, registration) = external(h.root("source", false), &auth);
    let request = prepare(
        &client,
        scope,
        &registration,
        auth,
        "history",
        IoMode::Pipe,
        "printf original",
    );
    let receipt = launch(&mut client, &request).await;
    settled(&mut client, &request).await;
    // Copy this test's settled journal to exercise actual restart validation in isolation.
    let history = h.temp.path().join("history");
    std::fs::create_dir(&history).unwrap();
    for entry in std::fs::read_dir(h.journal()).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), history.join(entry.file_name())).unwrap();
        }
    }
    let config = ExecutorConfig {
        journal_directory: history.clone(),
        helper_program: std::fs::canonicalize(env!("CARGO_BIN_EXE_calm-proc-supervisor")).unwrap(),
        kernel_client_secret: h.secret.clone(),
    };
    let output = history.join(format!("{}.output", request.identity.execution_id.as_str()));
    std::fs::write(&output, b"short").unwrap();
    let reopened = Executor::open(config.clone()).unwrap();
    let observation = reopened
        .dispatch(
            &h.secret,
            ExecutionRequest::Control(ExecutionControl {
                capability: capability(&request, &receipt, ExecutionControlPurpose::Observe),
                action: ProcessAction::Observe { from_cursor: None },
            }),
        )
        .await;
    assert!(matches!(observation, ExecutionReply::Observation {
        output_end: OutputEnd::Incomplete { reason }, cursor: 5, output, ..
    } if reason == "output_log_length_mismatch" && output == b"short"));
    // Neither startup nor a later Observe may turn a missing log into empty success.
    std::fs::remove_file(&output).unwrap();
    assert!(matches!(
        reopened
            .dispatch(
                &h.secret,
                ExecutionRequest::Control(ExecutionControl {
                    capability: capability(&request, &receipt, ExecutionControlPurpose::Observe),
                    action: ProcessAction::Observe { from_cursor: None },
                })
            )
            .await,
        ExecutionReply::Refused { .. }
    ));
    drop(reopened);
    assert!(Executor::open(config).is_err());
    assert!(!output.exists());
}
