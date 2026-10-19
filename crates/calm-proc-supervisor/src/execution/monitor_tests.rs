use super::*;
use std::{fs::File, io::Write, time::Duration};

#[tokio::test]
async fn retire_waits_output_open() {
    let temp = tempfile::tempdir().unwrap();
    let executor = Executor::open(ExecutorConfig {
        journal_directory: temp.path().join("journal"),
        helper_program: std::fs::canonicalize("/bin/sh").unwrap(),
        kernel_client_secret: CapabilityToken::fresh().unwrap(),
    })
    .unwrap();
    let (request, held) = protocol_tests::private(&executor, temp.path());
    protocol_tests::prepare(&executor, &request, &held).await;
    drop(held);
    let identity = request.identity.clone();
    let specification = request.specification.clone();
    let context = scope_helper::HelperContext {
        execution_id: identity.execution_id.clone(),
        executor_boot: executor.boot.clone(),
        nonce: CapabilityToken::fresh().unwrap(),
        mechanism: executor.mechanism.clone(),
    };
    // A real successfully spawned child owns its actual Child, pidfd and stdio. /bin/sh rejects
    // the helper envelope and exits; no payload or synthetic copy of process lifetime is involved.
    let output_log = output::OutputLog::open(&temp.path().join("output"), 1024).unwrap();
    let (process, native, real_reports) = process::spawn_unready(
        &executor.helper_image,
        &File::open("/").unwrap(),
        &specification,
        context.clone(),
        output_log.clone(),
    )
    .unwrap();
    drop(real_reports);
    let pid = native.pid;
    let receipt = ExecutionReceipt {
        identity: identity.clone(),
        slot_version: 1,
        process: native,
        scope_mechanism: executor.mechanism.clone(),
    };
    let lifecycle = request.control_tokens.lifecycle.clone();
    executor
        .persist(Journal {
            version: journal::JOURNAL_VERSION,
            host_boot_id: executor.host_boot_id.clone(),
            output_end: OutputEnd::Open,
            request,
            phase: Phase::Spawned(receipt),
            interactive_revoked: false,
            pending_persistence: false,
        })
        .unwrap();
    executor
        .processes
        .lock()
        .unwrap()
        .insert(identity.execution_id.clone(), process.clone());
    executor
        .outputs
        .lock()
        .unwrap()
        .insert(identity.execution_id.clone(), output_log);
    let (reports, mut writer) = process::pipe().unwrap();
    serde_json::to_writer(
        &mut writer,
        &scope_helper::HelperReport {
            context,
            event: scope_helper::HelperEvent::Drained { status: Some(0) },
        },
    )
    .unwrap();
    writer.write_all(b"\n").unwrap();
    drop(writer);
    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (reached_worker, release_worker) = (reached.clone(), release.clone());
    executor.monitor_with_reap_barrier(
        identity.execution_id.clone(),
        reports,
        process,
        async move {
            reached_worker.notify_one();
            release_worker.notified().await;
        },
    );
    tokio::time::timeout(Duration::from_secs(5), reached.notified())
        .await
        .unwrap();
    assert!(matches!(
        executor.record(&identity.execution_id).unwrap().phase,
        Phase::Drained { .. }
    ));
    assert_eq!(
        executor.record(&identity.execution_id).unwrap().output_end,
        OutputEnd::Open
    );
    assert_eq!(
        executor.store.load().unwrap()[0].output_end,
        OutputEnd::Open
    );
    protocol_tests::assert_released(&executor);
    assert!(matches!(
        executor
            .retire_execution(identity.execution_id.clone(), lifecycle.clone())
            .await
            .unwrap(),
        ExecutionReply::Busy(BusyReason::OutputOpen)
    ));
    assert!(
        executor
            .processes
            .lock()
            .unwrap()
            .contains_key(&identity.execution_id)
    );
    assert!(
        executor
            .outputs
            .lock()
            .unwrap()
            .contains_key(&identity.execution_id)
    );
    release.notify_one();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Retire discarded the concrete reap owner"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    loop {
        match executor
            .retire_execution(identity.execution_id.clone(), lifecycle.clone())
            .await
            .unwrap()
        {
            ExecutionReply::Retired(_) => break,
            ExecutionReply::Busy(BusyReason::OutputOpen) => {
                assert!(tokio::time::Instant::now() < deadline);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            other => panic!("retire failed: {other:?}"),
        }
    }
    assert!(
        !executor
            .records
            .lock()
            .unwrap()
            .contains_key(&identity.execution_id)
    );
    assert!(
        !executor
            .outputs
            .lock()
            .unwrap()
            .contains_key(&identity.execution_id)
    );
}

#[tokio::test]
async fn c2a_live_unknown_refuses_interaction_without_closing_writer() {
    use calm_session::control::IoMode;
    use std::os::unix::fs::MetadataExt;
    for mode in [IoMode::Pipe, IoMode::Pty { cols: 80, rows: 24 }] {
        let temp = tempfile::tempdir().unwrap();
        let secret = CapabilityToken::fresh().unwrap();
        let mut executor = Executor::open(ExecutorConfig {
            journal_directory: temp.path().join("journal"),
            helper_program: protocol_tests::helper(&temp),
            kernel_client_secret: secret.clone(),
        })
        .unwrap();
        let image = std::fs::metadata(std::env::current_exe().unwrap()).unwrap();
        Arc::get_mut(&mut executor).unwrap().mechanism = ScopeMechanismIdentity {
            executable_device: image.dev(),
            executable_inode: image.ino(),
            protocol_version: 1,
        };
        let (mut request, held) = protocol_tests::private(&executor, temp.path());
        request.specification.program = "/bin/sh".into();
        request.specification.args =
            vec!["-c".into(), "trap '' HUP; printf ready; read value".into()];
        request.specification.io_mode = mode;
        protocol_tests::prepare(&executor, &request, &held).await;
        let receipt = match executor
            .dispatch(
                &secret,
                ExecutionRequest::Launch(protocol_tests::consume(&request)),
            )
            .await
        {
            ExecutionReply::Receipt {
                receipt: Some(receipt),
                ..
            } => receipt,
            other => panic!("{other:?}"),
        };
        let id = &request.identity.execution_id;
        let process = executor.processes.lock().unwrap().get(id).unwrap().clone();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let output = executor
                .outputs
                .lock()
                .unwrap()
                .get(id)
                .unwrap()
                .snapshot(None)
                .unwrap()
                .1;
            if output.windows(5).any(|bytes| bytes == b"ready") {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // End the actual helper report stream without a drain proof. The monitor must
        // retain Unknown and the native Process; no synthetic phase transition.
        assert_eq!(
            unsafe { libc::kill(receipt.process.pid as i32, libc::SIGKILL) },
            0
        );
        loop {
            if matches!(executor.record(id).unwrap().phase, Phase::Unknown { .. }) {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(matches!(
            executor.store.load().unwrap()[0].phase,
            Phase::Unknown { .. }
        ));
        assert!(executor.processes.lock().unwrap().contains_key(id));
        let capability = ExecutionCapability {
            identity: receipt.identity,
            slot_version: receipt.slot_version,
            token: request.control_tokens.interactive.clone(),
            purpose: ExecutionControlPurpose::Interactive,
        };
        // Clean up even when testing the intentionally broken production mutation.
        let mut replies = Vec::new();
        for action in [
            ProcessAction::CloseStdin,
            ProcessAction::WriteStdin { bytes: vec![] },
            ProcessAction::Resize {
                cols: 90,
                rows: 30,
                pixel_w: 0,
                pixel_h: 0,
            },
        ] {
            replies.push(
                executor
                    .dispatch(
                        &secret,
                        ExecutionRequest::Control(ExecutionControl {
                            capability: capability.clone(),
                            action,
                        }),
                    )
                    .await,
            );
        }
        let writer_open = process.input.lock().unwrap().is_some();
        // Bypass state admission only for test cleanup; release the blocked target.
        let _ = process.write(b"done\n");
        process.input.lock().unwrap().take();
        for reply in replies {
            assert!(matches!(reply, ExecutionReply::Refused { .. }), "{reply:?}");
        }
        assert!(writer_open, "Unknown refusal closed the original writer");
    }
}
