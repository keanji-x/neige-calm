#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::Duration;

use calm_terminal_runtime::{RuntimeLaunch, connect};
use rmux_sdk::{EnsureSession, PaneRecoveryEvent, Rmux, SessionName, TerminalSizeSpec};
use tempfile::TempDir;

const BUDGET: Duration = Duration::from_secs(5);

struct Host {
    child: Child,
    root: TempDir,
    socket: PathBuf,
}

fn private_root() -> std::io::Result<TempDir> {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir()?;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
    Ok(root)
}

fn launch(root: &TempDir) -> RuntimeLaunch {
    RuntimeLaunch {
        executable: env!("CARGO_BIN_EXE_neige-terminal-runtime").into(),
        socket: root.path().join("runtime.sock"),
        cwd: root.path().to_owned(),
        home: root.path().to_owned(),
        executable_path: "/usr/bin:/bin".into(),
        locale: "C.UTF-8".into(),
    }
}

impl Host {
    async fn start(root: TempDir) -> anyhow::Result<(Self, Rmux)> {
        let config = launch(&root);
        let log = std::fs::File::create(root.path().join("runtime.log"))?;
        let child = config
            .command()?
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()?;
        let mut host = Self {
            child,
            root,
            socket: config.socket,
        };
        let client = tokio::time::timeout(BUDGET, async {
            loop {
                if let Some(status) = host.child.try_wait()? {
                    anyhow::bail!(
                        "runtime exited during startup: {status}; {}",
                        std::fs::read_to_string(host.root.path().join("runtime.log"))?
                    );
                }
                if let Ok(client) = connect(&host.socket, BUDGET).await {
                    return Ok::<_, anyhow::Error>(client);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await??;
        Ok((host, client))
    }

    async fn shutdown(&mut self, client: Rmux) -> anyhow::Result<()> {
        tokio::time::timeout(BUDGET, client.shutdown()).await??;
        let status = tokio::time::timeout(BUDGET, async {
            loop {
                if let Some(status) = self.child.try_wait()? {
                    return Ok::<_, std::io::Error>(status);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await??;
        anyhow::ensure!(status.success(), "runtime shutdown failed: {status}");
        anyhow::ensure!(
            !self.socket.exists(),
            "socket must be removed on graceful shutdown"
        );
        Ok(())
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // Failure cleanup for this test-owned host only; no global daemon is
        // discovered or stopped. Normal tests use the SDK shutdown above.
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[tokio::test]
async fn runtime_shell_round_trip_reconnect_and_exit() -> anyhow::Result<()> {
    let (mut host, client) = Host::start(private_root()?).await?;
    let create = || {
        EnsureSession::try_named("terminal-1").unwrap().create_only().detached(true)
        .size(TerminalSizeSpec::new(80, 24)).argv([
            "/bin/sh", "-c",
            "printf 'ready\\n'; IFS= read -r line; printf 'received:%s\\n' \"$line\"; IFS= read -r finish; printf 'last-line\\n'; exit 7",
        ])
    };
    let session = client.ensure_session(create()).await?;
    assert!(
        client.ensure_session(create()).await.is_err(),
        "create-only must not reuse a session"
    );
    let pane = session.pane(0, 0);
    pane.wait_for_text("ready").await?;
    let mut info = pane.info().await?;
    assert_eq!(info.panes.len(), 1);
    let identity = info.panes.remove(0);
    pane.send_text("中文\n").await?;
    pane.wait_for_text("received:中文").await?;
    let mut recovery = pane.recover_output().await?;
    let first = tokio::time::timeout(BUDGET, recovery.next()).await??;
    assert!(
        matches!(first, Some(PaneRecoveryEvent::Rebase(ref frame)) if !frame.keyframe.is_empty())
    );
    drop(recovery);
    drop(pane);
    drop(session);
    drop(client);

    let client = connect(&host.socket, BUDGET).await?;
    let session = client.session(SessionName::new("terminal-1")?).await?;
    let pane = session.pane_by_id(identity.id).await?;
    let mut info = pane.info().await?;
    assert_eq!(info.panes.len(), 1);
    let reconnected = info.panes.remove(0);
    assert_eq!(reconnected.id, identity.id);
    assert_eq!(reconnected.generation, identity.generation);
    assert!(
        pane.snapshot()
            .await?
            .visible_lines()
            .join("\n")
            .contains("received:中文")
    );
    pane.send_text("finish\n").await?;
    let exit = tokio::time::timeout(BUDGET, pane.wait_exit())
        .await??
        .expect("retained exit details");
    assert_eq!(exit.code, Some(7));
    assert_eq!(exit.signal, None);
    assert!(
        pane.snapshot()
            .await?
            .visible_lines()
            .join("\n")
            .contains("last-line")
    );
    session.kill().await?;
    assert!(!client.has_session(SessionName::new("terminal-1")?).await?);
    host.shutdown(client).await
}

#[tokio::test]
async fn runtime_does_not_load_user_configuration() -> anyhow::Result<()> {
    let root = private_root()?;
    std::fs::write(
        root.path().join(".rmux.conf"),
        "run-shell 'printf configured > configuration-ran'\n",
    )?;
    let (mut host, client) = Host::start(root).await?;
    // A real create/read round trip gives startup configuration a chance to
    // finish. Configuration is disabled at the production host constructor.
    let session = client
        .ensure_session(
            EnsureSession::try_named("config-probe")?
                .create_only()
                .detached(true)
                .argv(["/bin/sh", "-c", "printf 'ready\\n'; read line"]),
        )
        .await?;
    session.pane(0, 0).wait_for_text("ready").await?;
    assert!(!host.root.path().join("configuration-ran").exists());
    host.shutdown(client).await
}

#[tokio::test]
async fn runtime_second_host_cannot_replace_live_endpoint() -> anyhow::Result<()> {
    let (mut host, client) = Host::start(private_root()?).await?;
    let output = launch(&host.root).command()?.output()?;
    assert!(!output.status.success());
    assert!(
        connect(&host.socket, BUDGET)
            .await?
            .list_sessions()
            .await?
            .is_empty()
    );
    host.shutdown(client).await
}

#[tokio::test]
async fn runtime_missing_endpoint_connect_never_starts_a_daemon() -> anyhow::Result<()> {
    let root = private_root()?;
    let socket = root.path().join("missing.sock");
    assert!(connect(&socket, Duration::from_millis(100)).await.is_err());
    assert!(!socket.exists());
    assert_eq!(std::fs::read_dir(root.path())?.count(), 0);
    Ok(())
}

#[test]
fn runtime_launch_uses_explicit_environment() -> anyhow::Result<()> {
    let root = private_root()?;
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_terminal-runtime-parent-probe"))
        .arg(root.path())
        .env("NEIGE_TEST_PARENT_SENTINEL", "must-not-leak")
        .env("RMUX_CONFIG_FILE", "/not/a/real/config")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn runtime_refuses_public_socket_directory() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let root = private_root()?;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755))?;
    let result = launch(&root).command();
    assert!(matches!(result, Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied));
    Ok(())
}

#[test]
fn runtime_refuses_existing_endpoint_without_unlinking_it() -> anyhow::Result<()> {
    let root = private_root()?;
    let config = launch(&root);
    std::fs::write(&config.socket, b"owned by someone else")?;
    let output = config.command()?.output()?;
    assert!(!output.status.success());
    assert_eq!(std::fs::read(&config.socket)?, b"owned by someone else");
    Ok(())
}
