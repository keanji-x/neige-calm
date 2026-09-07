#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::Duration;

use calm_terminal_runtime::{RuntimeClient, RuntimeLaunch, TerminalSpec, connect};
use rmux_sdk::PaneRecoveryEvent;
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

fn terminal_spec(root: &std::path::Path, name: &str, script: &str) -> TerminalSpec {
    TerminalSpec {
        name: name.into(),
        argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
        cwd: root.to_owned(),
        cols: 80,
        rows: 24,
        environment: std::collections::BTreeMap::from([
            ("HOME".into(), root.to_str().unwrap().into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("LANG".into(), "C.UTF-8".into()),
        ]),
    }
}

impl Host {
    async fn start(root: TempDir) -> anyhow::Result<(Self, RuntimeClient)> {
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

    async fn shutdown(&mut self, client: RuntimeClient) -> anyhow::Result<()> {
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
        terminal_spec(
            host.root.path(),
            "terminal-1",
            "printf 'ready\\n'; IFS= read -r line; printf 'received:%s\\n' \"$line\"; IFS= read -r finish; printf 'last-line\\n'; exit 7",
        )
    };
    let pane = client.create(create()).await?;
    assert!(
        client.create(create()).await.is_err(),
        "create-only must not reuse a session"
    );
    pane.wait_for_text("ready").await?;
    let identity = pane.info().await?;
    pane.send_text("中文\n").await?;
    pane.wait_for_text("received:中文").await?;
    let mut recovery = pane.recover_output().await?;
    let first = tokio::time::timeout(BUDGET, recovery.next()).await??;
    assert!(
        matches!(first, Some(PaneRecoveryEvent::Rebase(ref frame)) if !frame.keyframe.is_empty())
    );
    drop(recovery);
    drop(pane);
    drop(client);

    let client = connect(&host.socket, BUDGET).await?;
    let pane = client.attach_existing("terminal-1").await?;
    let reconnected = pane.info().await?;
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
    pane.close().await?;
    assert!(!client.has_session("terminal-1").await?);
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
    let pane = client
        .create(terminal_spec(
            host.root.path(),
            "config-probe",
            "printf 'ready\\n'; read line",
        ))
        .await?;
    pane.wait_for_text("ready").await?;
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
fn runtime_shell_does_not_inherit_calling_application_environment() -> anyhow::Result<()> {
    let root = private_root()?;
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_terminal-runtime-parent-probe"))
        .arg("--pane-probe")
        .arg(root.path())
        .arg(env!("CARGO_BIN_EXE_neige-terminal-runtime"))
        .env("NEIGE_TEST_PARENT_SENTINEL", "must-not-leak")
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

#[tokio::test]
async fn runtime_create_preserves_literal_cwd_argv_and_explicit_environment() -> anyhow::Result<()>
{
    let (mut host, client) = Host::start(private_root()?).await?;
    let cwd = host.root.path().join("work #{session_name}");
    std::fs::create_dir(&cwd)?;
    let mut spec = terminal_spec(
        &cwd,
        "literal-probe",
        "printf 'cwd:%s\\narg:%s\\nexplicit:%s\\n' \"$PWD\" \"$1\" \"$NEIGE_EXPLICIT\"; read line",
    );
    let argument = "spaces ; $(not-a-command)";
    spec.argv.extend(["probe-shell".into(), argument.into()]);
    spec.environment
        .insert("NEIGE_EXPLICIT".into(), "intentional".into());
    let pane = client.create(spec).await?;
    pane.wait_for_text("explicit:intentional").await?;
    let text = pane.snapshot().await?.visible_lines().join("\n");
    assert!(
        text.contains(cwd.to_str().unwrap()),
        "literal working directory must survive tmux format expansion"
    );
    assert!(
        text.contains(argument),
        "argv must not be reinterpreted as shell text"
    );
    host.shutdown(client).await
}

#[tokio::test]
async fn runtime_invalid_creation_is_rejected_without_side_effects() -> anyhow::Result<()> {
    use calm_terminal_runtime::CreateError;
    let (mut host, client) = Host::start(private_root()?).await?;
    let mut spec = terminal_spec(host.root.path(), "bad-geometry", "read line");
    spec.cols = 0;
    assert!(matches!(
        client.create(spec).await,
        Err(CreateError::Invalid(_))
    ));
    let mut spec = terminal_spec(host.root.path(), "bad-environment", "read line");
    spec.environment.insert("BAD=KEY".into(), "value".into());
    assert!(matches!(
        client.create(spec).await,
        Err(CreateError::Invalid(_))
    ));
    assert!(client.list_sessions().await?.is_empty());
    host.shutdown(client).await
}

#[test]
fn runtime_creation_timeout_is_unknown_and_late_creation_is_reconcilable() -> anyhow::Result<()> {
    use calm_terminal_runtime::CreateError;
    // Saturate one blocking-worker slot after connecting. The actual create
    // request is queued until its caller times out, then allowed to execute.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let (mut host, client) = Host::start(private_root()?).await?;
        let (release, held) = std::sync::mpsc::channel();
        let (armed, ready) = tokio::sync::oneshot::channel();
        let holder = tokio::task::spawn_blocking(move || {
            let _ = armed.send(());
            let _ = held.recv();
        });
        ready.await?;
        let result = client
            .create(terminal_spec(
                host.root.path(),
                "late-create",
                "printf 'ready\\n'; read line",
            ))
            .await;
        release.send(())?;
        holder.await?;
        assert!(
            matches!(result, Err(CreateError::OutcomeUnknown { ref name }) if name == "late-create")
        );
        let pane = tokio::time::timeout(BUDGET, async {
            loop {
                if let Ok(pane) = client.attach_existing("late-create").await {
                    return pane;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;
        pane.wait_for_text("ready").await?;
        assert_eq!(client.list_sessions().await?, vec!["late-create"]);
        host.shutdown(client).await
    })
}
