use super::*;
use calm_session::control::IoMode;
use std::{fs::File, time::Duration};
use workspace::identity::WorkspaceCwd;

#[tokio::test]
async fn stop_before_started_deadline_and_shutdown() {
    let temp = tempfile::tempdir().unwrap();
    let executor = Executor::open(ExecutorConfig {
        journal_directory: temp.path().join("journal"),
        helper_program: std::env::current_exe().unwrap(),
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
        version: calm_session::control::execution::SpecificationVersion::AmbientV2,
        ambient_environment: calm_session::control::execution::AmbientEnvironment::None,
        program: "/bin/sh".into(),
        args: vec!["-c".into(), "trap '' TERM; printf ready; sleep 3".into()],
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
    // The shim only routes the normal bootstrap FDs into this test binary's isolated
    // subprocess entry. The real helper implementation owns spawning, signals and drain.
    use std::os::unix::fs::PermissionsExt;
    let shim = temp.path().join("helper-entry");
    let executable = std::env::current_exe().unwrap();
    let quoted = executable.to_str().unwrap().replace('\'', "'\"'\"'");
    let body = temp.path().join("helper-entry-body");
    // The interpreter reads the named body; the bootstrap executable FD stays CLOEXEC.
    std::fs::write(&shim, format!("#!/bin/sh {}\n", body.display())).unwrap();
    std::fs::write(
        &body,
        format!(
            "E1A_CONFIG_FD=$3 E1A_REPORT_FD=$4 exec '{}' --ignored --exact {} --nocapture\n",
            quoted, "execution::stop_tests::startup_barrier_helper_entry"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700)).unwrap();
    let output_log = output::OutputLog::open(&temp.path().join("output"), 1024).unwrap();
    let (process, native, real_reports) = process::spawn(
        &File::open(&shim).unwrap(),
        &File::open("/").unwrap(),
        &specification,
        context.clone(),
        output_log.clone(),
    )
    .unwrap();

    let pid = native.pid();
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
        action: ProcessAction::Stop,
    };
    executor
        .persist(Journal {
            version: journal::JOURNAL_VERSION,
            host_boot_id: executor.host_boot_id.clone(),
            output_end: OutputEnd::Open,
            request: PrepareExecution {
                source: PreparedSource::Registered,
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
    // The real target is blocked in pre_exec for 1000ms unless KILL interrupts it.
    // This leaves std's spawn handshake pending, so the helper cannot report Started.
    let start = tokio::time::Instant::now();
    while !executor
        .outputs
        .lock()
        .unwrap()
        .get(&identity.execution_id)
        .unwrap()
        .snapshot(None)
        .unwrap()
        .1
        .windows(7)
        .any(|bytes| bytes == b"barrier")
    {
        assert!(start.elapsed() < Duration::from_secs(2));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(!process.started.load(std::sync::atomic::Ordering::SeqCst));
    let accepted = tokio::time::Instant::now();
    executor.control(control.clone()).await.unwrap();
    let deadline = *process.stop_deadline.lock().unwrap();
    let shutdown_executor = executor.clone();
    let shutdown = tokio::spawn(async move {
        shutdown_executor.graceful_shutdown().await;
    });
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            },
            0
        );
        if unsafe { info.si_pid() } != 0 {
            break;
        }
        assert!(
            accepted.elapsed() < Duration::from_millis(600),
            "accepted Stop waited for Started"
        );
        executor.control(control.clone()).await.unwrap();
        assert_eq!(
            *process.stop_deadline.lock().unwrap(),
            deadline,
            "repeated Stop rearmed deadline"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(accepted.elapsed() >= Duration::from_millis(200));
    executor.monitor(identity.execution_id.clone(), real_reports, process);
    shutdown.await.unwrap();
    assert!(matches!(
        executor.record(&identity.execution_id).unwrap().phase,
        Phase::Drained { .. }
    ));
}

// Executed only as an isolated helper subprocess by the regression above.
#[test]
#[ignore = "isolated startup-barrier helper entry"]
fn startup_barrier_helper_entry() {
    use std::{
        io::Write,
        os::{fd::AsRawFd, unix::process::CommandExt},
    };
    let config = std::env::var("E1A_CONFIG_FD").unwrap().parse().unwrap();
    let report = std::env::var("E1A_REPORT_FD").unwrap().parse().unwrap();
    scope_helper::run_with_command_hook(config, report, |command| {
        let (read, write) = process::pipe().unwrap();
        let write_fd = write.as_raw_fd();
        // The parent helper retains the writer while its target waits in the exec handshake.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1000));
            drop(write);
        });
        unsafe {
            command.pre_exec(move || {
                // All operations after fork are async-signal-safe. TERM may interrupt read,
                // but only the barrier release or scope KILL ends the startup wait.
                libc::close(write_fd);
                libc::write(1, b"barrier".as_ptr().cast(), 7);
                let mut byte = [0u8];
                loop {
                    let result = libc::read(read.as_raw_fd(), byte.as_mut_ptr().cast(), 1);
                    if result >= 0 {
                        return Ok(());
                    }
                    let error = std::io::Error::last_os_error();
                    if error.kind() != std::io::ErrorKind::Interrupted {
                        return Err(error);
                    }
                }
            });
        }
    })
    .unwrap();
    std::io::stdout().flush().unwrap();
}
