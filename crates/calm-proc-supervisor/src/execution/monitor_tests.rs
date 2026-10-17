use super::*;
use calm_session::control::IoMode;
use std::{fs::File, io::Write, time::Duration};
use workspace::identity::WorkspaceCwd;

#[tokio::test]
async fn execution_monitor_cleanup_after_drained_before_wait_still_reaps_exact_child() {
    let temp = tempfile::tempdir().unwrap();
    let executor = Executor::open(ExecutorConfig {
        journal_directory: temp.path().join("journal"),
        helper_program: std::fs::canonicalize("/bin/sh").unwrap(),
        kernel_client_secret: CapabilityToken::fresh().unwrap(),
    })
    .unwrap();
    let identity = ExecutionIdentity {
        execution_id: ExecutionId::fresh().unwrap(),
        executor_boot: executor.boot.clone(),
        ticket_id: LaunchTicketId::fresh().unwrap(),
        source_registration: ObjectRegistrationId::fresh().unwrap(),
        source_incarnation: SourceObjectIncarnation::ExternalDir(
            ExternalDirectoryIncarnationId::fresh().unwrap(),
        ),
        authority: ExecutionAuthority {
            grant_id: ExecutionGrantId::fresh().unwrap(),
            authorization_binding: AuthorizationBindingId::fresh().unwrap(),
            authorization_version: 1,
        },
        scope: ExecutionScope::ExternalDir {
            directory_id: ExternalDirectoryId::fresh().unwrap(),
            registration_version: 1,
            cwd: WorkspaceCwd::root(),
        },
        operation_id: "monitor-owned-child".into(),
        specification_id: SpecificationId::fresh().unwrap(),
        process_alias: "monitor-owned-child".into(),
    };
    let specification = ExecutionSpecification {
        program: "/bin/true".into(),
        args: vec![],
        environment: vec![],
        io_mode: IoMode::Pipe,
        replay_bytes: 1024,
    };
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
    let tokens = ExecutionControlTokens::fresh().unwrap();
    let control = ExecutionControl {
        capability: ExecutionCapability {
            identity: identity.clone(),
            slot_version: 1,
            token: tokens.lifecycle.clone(),
            purpose: ExecutionControlPurpose::Lifecycle,
        },
        action: ProcessAction::Cleanup,
    };
    executor
        .persist(Journal {
            version: journal::JOURNAL_VERSION,
            host_boot_id: executor.host_boot_id.clone(),
            output_end: OutputEnd::Open,
            request: PrepareExecution {
                identity: identity.clone(),
                ticket: CapabilityToken::fresh().unwrap(),
                control_tokens: tokens,
                handoff: None,
                specification,
            },
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
    assert!(matches!(
        executor.control(control).await.unwrap(),
        ExecutionReply::ActionComplete
    ));
    assert!(
        !executor
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
            "Cleanup discarded the concrete reap owner"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
