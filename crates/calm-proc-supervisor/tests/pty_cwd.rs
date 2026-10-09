use calm_session::control::{
    AttachRequest, CleanupRequest, ControlErrorKind, ControlMsg, ControlReply, EnsureProcRequest,
    IoMode,
};
use calm_session::{read_frame, write_frame};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;

struct Supervisor {
    child: tokio::process::Child,
    socket: std::path::PathBuf,
}

impl Supervisor {
    async fn start(root: &Path) -> anyhow::Result<Self> {
        let socket = calm_test_sockets::socket_path(root, "supervisor.sock");
        let home = root.join("home");
        std::fs::create_dir(&home)?;
        let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_calm-proc-supervisor"))
            .args(["--control-sock", socket.to_str().unwrap()])
            .current_dir(root)
            .env("HOME", home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if UnixStream::connect(&socket).await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        Ok(Self { child, socket })
    }

    async fn ensure(&self, cwd: &str, marker: &Path) -> anyhow::Result<ControlReply> {
        let mut stream = UnixStream::connect(&self.socket).await?;
        write_frame(
            &mut stream,
            &ControlMsg::EnsureProc(EnsureProcRequest {
                proc_id: "cwd-check".into(),
                program: "/bin/sh".into(),
                args: vec![
                    "-c".into(),
                    "pwd -P > \"$1\"".into(),
                    "cwd-check".into(),
                    marker.display().to_string(),
                ],
                envs: Vec::new(),
                cwd: cwd.into(),
                ready_timeout_ms: 0,
                io_mode: IoMode::Pty { cols: 80, rows: 24 },
                replay_bytes: 8192,
            }),
        )
        .await?;
        let reply = receive(&mut stream).await?;
        if matches!(reply, ControlReply::Spawned { .. }) {
            assert!(matches!(receive(&mut stream).await?, ControlReply::Ready));
            // Join the real child exit before checking its filesystem side effect,
            // including on the unfixed path, so absence is not a timing assertion.
            write_frame(
                &mut stream,
                &ControlMsg::Attach(AttachRequest {
                    proc_id: "cwd-check".into(),
                    from_cursor: None,
                    reader_id: "cwd-reader".into(),
                }),
            )
            .await?;
            loop {
                match receive(&mut stream).await? {
                    ControlReply::AttachOk(_) | ControlReply::Output { .. } => {}
                    ControlReply::Exited { status, .. } => {
                        assert_eq!(status, Some(0));
                        break;
                    }
                    other => anyhow::bail!("unexpected attach reply: {other:?}"),
                }
            }
        }
        Ok(reply)
    }

    async fn assert_unregistered(&self) -> anyhow::Result<()> {
        let mut stream = UnixStream::connect(&self.socket).await?;
        write_frame(
            &mut stream,
            &ControlMsg::Cleanup(CleanupRequest {
                proc_id: "cwd-check".into(),
            }),
        )
        .await?;
        assert!(matches!(
            receive(&mut stream).await?,
            ControlReply::Error {
                kind: ControlErrorKind::UnknownProc,
                ..
            }
        ));
        let mut stream = UnixStream::connect(&self.socket).await?;
        write_frame(
            &mut stream,
            &ControlMsg::Attach(AttachRequest {
                proc_id: "cwd-check".into(),
                from_cursor: None,
                reader_id: "absent-reader".into(),
            }),
        )
        .await?;
        assert!(matches!(
            receive(&mut stream).await?,
            ControlReply::Error {
                kind: ControlErrorKind::UnknownProc,
                ..
            }
        ));
        Ok(())
    }
}

async fn receive(stream: &mut UnixStream) -> anyhow::Result<ControlReply> {
    Ok(tokio::time::timeout(Duration::from_secs(10), read_frame(stream)).await??)
}

async fn rejects_cwd(file: bool) -> anyhow::Result<()> {
    let root = calm_test_sockets::socket_dir("pty-cwd");
    let cwd = root.path().join("invalid");
    if file {
        std::fs::write(&cwd, b"not a directory")?;
    }
    let marker = root.path().join("executed");
    let mut supervisor = Supervisor::start(root.path()).await?;
    let reply = supervisor.ensure(cwd.to_str().unwrap(), &marker).await?;
    assert!(
        matches!(&reply, ControlReply::SpawnFailed { error, child_already_reaped: false, disposition: calm_session::control::SpawnFailedDisposition::NoChildCreated }
            if error.contains(cwd.to_str().unwrap()) && error.contains("not a directory")),
        "expected visible cwd rejection, got {reply:?}; command executed: {}",
        marker.exists()
    );
    assert!(
        !marker.exists(),
        "rejected command ran in HOME or another cwd"
    );
    supervisor.assert_unregistered().await?;
    // The same proc id remains usable after rejection; cleanup did not damage it.
    assert!(matches!(
        supervisor
            .ensure(root.path().to_str().unwrap(), &marker)
            .await?,
        ControlReply::Spawned { .. }
    ));
    assert_eq!(
        std::fs::read_to_string(marker)?.trim(),
        root.path().canonicalize()?.to_str().unwrap()
    );
    supervisor.child.kill().await?;
    Ok(())
}

#[tokio::test]
async fn pty_cwd_missing_directory_is_rejected_without_execution_or_registration()
-> anyhow::Result<()> {
    rejects_cwd(false).await
}

#[tokio::test]
async fn pty_cwd_regular_file_is_rejected_without_execution_or_registration() -> anyhow::Result<()>
{
    rejects_cwd(true).await
}

async fn accepts_cwd(relative: bool, empty: bool) -> anyhow::Result<()> {
    let root = calm_test_sockets::socket_dir("pty-cwd");
    let directory = root.path().join("directory");
    std::fs::create_dir(&directory)?;
    let cwd = if empty {
        ""
    } else if relative {
        "directory"
    } else {
        directory.to_str().unwrap()
    };
    let marker = root.path().join("executed");
    let mut supervisor = Supervisor::start(root.path()).await?;
    assert!(matches!(
        supervisor.ensure(cwd, &marker).await?,
        ControlReply::Spawned { .. }
    ));
    let expected = if empty {
        root.path().join("home")
    } else {
        directory
    };
    assert_eq!(
        std::fs::read_to_string(marker)?.trim(),
        expected.canonicalize()?.to_str().unwrap()
    );
    supervisor.child.kill().await?;
    Ok(())
}

#[tokio::test]
async fn pty_cwd_valid_directory_runs_in_requested_directory() -> anyhow::Result<()> {
    accepts_cwd(false, false).await
}

#[tokio::test]
async fn pty_cwd_relative_directory_resolves_against_supervisor_directory() -> anyhow::Result<()> {
    accepts_cwd(true, false).await
}

#[tokio::test]
async fn pty_cwd_empty_preserves_home_default() -> anyhow::Result<()> {
    accepts_cwd(false, true).await
}

#[tokio::test]
async fn pty_cwd_validation_keeps_pipe_bootstrap_missing_directory_contract() -> anyhow::Result<()>
{
    let root = calm_test_sockets::socket_dir("pipe-cwd");
    let missing = root.path().join("daemon-will-create");
    let supervisor = calm_proc_supervisor::test_support::InProcessProcSupervisor::start().await?;
    let mut stream = UnixStream::connect(supervisor.sock()).await?;
    write_frame(
        &mut stream,
        &ControlMsg::EnsureProc(EnsureProcRequest {
            proc_id: "pipe-cwd".into(),
            program: env!("CARGO_BIN_EXE_proc-supervisor-ready-sleeper").into(),
            args: vec!["--ready-fd".into(), "0".into()],
            envs: Vec::new(),
            cwd: missing.display().to_string(),
            ready_timeout_ms: 10_000,
            io_mode: IoMode::Pipe,
            replay_bytes: 0,
        }),
    )
    .await?;
    assert!(matches!(
        receive(&mut stream).await?,
        ControlReply::Spawned { .. }
    ));
    assert!(matches!(receive(&mut stream).await?, ControlReply::Ready));
    assert!(
        !missing.exists(),
        "supervisor must leave cwd creation to daemon"
    );
    Ok(())
}

#[tokio::test]
async fn pipe_spawn_error_is_unknown_but_pre_spawn_error_is_definite() -> anyhow::Result<()> {
    let supervisor = calm_proc_supervisor::test_support::InProcessProcSupervisor::start().await?;
    for (args, expected) in [
        (
            vec!["--ready-fd".into(), "0".into()],
            calm_session::control::SpawnFailedDisposition::Unknown,
        ),
        (
            vec![],
            calm_session::control::SpawnFailedDisposition::NoChildCreated,
        ),
    ] {
        let mut stream = UnixStream::connect(supervisor.sock()).await?;
        write_frame(
            &mut stream,
            &ControlMsg::EnsureProc(EnsureProcRequest {
                proc_id: "pipe-spawn-error".into(),
                program: "/nonexistent/neige-pipe-test-program".into(),
                args,
                envs: vec![],
                cwd: "/tmp".into(),
                ready_timeout_ms: 100,
                io_mode: IoMode::Pipe,
                replay_bytes: 0,
            }),
        )
        .await?;
        let reply = receive(&mut stream).await?;
        assert!(
            matches!(reply, ControlReply::SpawnFailed { disposition, child_already_reaped: false, .. } if disposition == expected),
            "{reply:?}"
        );
    }
    Ok(())
}
