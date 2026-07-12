#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewCard, NewCove, NewTerminal, NewWave, Terminal};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes::theme::RequestTheme;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::terminal_renderer::RendererConfig;
use calm_session::DaemonMsg;
use serde_json::json;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::timeout;

#[tokio::test]
async fn stale_terminal_row_marked_exited() {
    let fixture = TestFixture::boot_with_supervisor().await;
    let term = fixture.seed_terminal().await;

    calm_server::reconcile_supervisor_on_boot(&fixture.state).await;

    let got = fixture
        .repo
        .terminal_get(&term.id)
        .await
        .expect("terminal_get")
        .expect("terminal row");
    assert_eq!(got.exit_code, Some(-1));
}

#[tokio::test]
async fn live_terminal_left_alone() {
    let fixture = TestFixture::boot_with_supervisor().await;
    let term = fixture.seed_terminal().await;
    let entry = fixture.ensure_live_renderer(&term).await;
    let mut events = entry
        .take_initial_event_rx()
        .expect("initial renderer event receiver");
    wait_for_child_ready(&mut events).await;

    calm_server::reconcile_supervisor_on_boot(&fixture.state).await;

    let got = fixture
        .repo
        .terminal_get(&term.id)
        .await
        .expect("terminal_get")
        .expect("terminal row");
    assert_eq!(got.exit_code, None);
    assert!(fixture.state.terminal_renderer.get(&term.id).is_some());
    fixture.state.terminal_renderer.drop_entry(&term.id).await;
}

#[tokio::test]
async fn probe_error_leaves_row_unchanged() {
    let fixture = TestFixture::boot_with_missing_supervisor().await;
    let term = fixture.seed_terminal().await;

    calm_server::reconcile_supervisor_on_boot(&fixture.state).await;

    let got = fixture
        .repo
        .terminal_get(&term.id)
        .await
        .expect("terminal_get")
        .expect("terminal row");
    assert_eq!(got.exit_code, None);
}

struct TestFixture {
    _tmp: TempDir,
    repo: Arc<dyn Repo>,
    state: AppState,
    supervisor: Option<Child>,
}

impl TestFixture {
    async fn boot_with_supervisor() -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let control_sock = tmp.path().join("proc-supervisor.sock");
        let supervisor = spawn_real_supervisor(&control_sock).await;
        Self::boot(tmp, Some(control_sock), Some(supervisor)).await
    }

    async fn boot_with_missing_supervisor() -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let control_sock = tmp.path().join("missing-proc-supervisor.sock");
        Self::boot(tmp, Some(control_sock), None).await
    }

    async fn boot(
        tmp: TempDir,
        proc_supervisor_sock: Option<PathBuf>,
        supervisor: Option<Child>,
    ) -> Self {
        let repo: Arc<dyn Repo> = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        let events = EventBus::new();
        let state = AppState::from_parts(
            repo.clone(),
            events.clone(),
            Arc::new(DaemonClient {
                data_dir: tmp.path().to_path_buf(),
                proc_supervisor_sock,
            }),
            Arc::new(PluginHost::new_full(
                Arc::new(PluginRegistry::empty()),
                repo.clone(),
                PathBuf::new(),
                tmp.path().join("plugins-data"),
                Vec::new(),
                events,
                calm_server::state::WriteContext::new(
                    calm_server::card_role_cache::CardRoleCache::new(),
                    calm_server::wave_cove_cache::WaveCoveCache::new(),
                ),
            )),
            Arc::new(CodexClient::new_stub()),
            None,
            None,
        );
        Self {
            _tmp: tmp,
            repo,
            state,
            supervisor,
        }
    }

    async fn seed_terminal(&self) -> Terminal {
        let cove = self
            .repo
            .cove_create(NewCove {
                name: "reconcile-e2e".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .expect("create cove");
        let wave = self
            .repo
            .wave_create(NewWave {
                workflow_input: None,
                cove_id: cove.id,
                title: "reconcile-e2e".into(),
                sort: None,
                cwd: workspace_root().display().to_string(),
                workflow_id: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .expect("create wave");
        let card = self
            .repo
            .card_create(NewCard {
                wave_id: wave.id,
                title: None,
                kind: "terminal".into(),
                sort: None,
                payload: json!({}),
            })
            .await
            .expect("create card");
        self.repo
            .terminal_create(NewTerminal {
                card_id: card.id,
                program: "sleep 30".into(),
                cwd: workspace_root().display().to_string(),
                env: json!({}),
                theme: RequestTheme::default_dark(),
            })
            .await
            .expect("create terminal")
    }

    async fn ensure_live_renderer(
        &self,
        term: &Terminal,
    ) -> Arc<calm_server::terminal_renderer::RendererEntry> {
        let supervisor_sock = self
            .state
            .daemon
            .proc_supervisor_sock
            .clone()
            .expect("supervisor sock");
        self.state
            .terminal_renderer
            .ensure(RendererConfig {
                terminal_id: term.id.clone(),
                cols: 80,
                rows: 24,
                buffer_bytes: 1 << 20,
                terminal_fg: (216, 219, 226),
                terminal_bg: (15, 20, 24),
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "echo ready; sleep 30".into()],
                envs: std::env::vars().collect(),
                cwd: workspace_root().display().to_string(),
                supervisor_sock,
            })
            .await
            .expect("ensure renderer")
    }
}

impl Drop for TestFixture {
    fn drop(&mut self) {
        if let Some(supervisor) = self.supervisor.as_mut() {
            let _ = supervisor.start_kill();
        }
    }
}

async fn wait_for_child_ready(rx: &mut tokio::sync::broadcast::Receiver<DaemonMsg>) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for ChildReady");
        let msg = timeout(remaining, rx.recv())
            .await
            .expect("child ready timeout")
            .expect("event channel closed");
        if matches!(msg, DaemonMsg::ChildReady { .. }) {
            return;
        }
    }
}

async fn spawn_real_supervisor(control_sock: &Path) -> Child {
    let mut child = Command::new(locate_bin("calm-proc-supervisor"))
        .arg("--control-sock")
        .arg(control_sock)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn supervisor");
    wait_until_listening(control_sock, &mut child).await;
    assert!(child.id().is_some(), "supervisor exited before listening");
    child
}

async fn wait_until_listening(sock: &Path, child: &mut Child) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if UnixStream::connect(sock).await.is_ok() {
            return;
        }
        if let Some(status) = child.try_wait().expect("poll supervisor child") {
            panic!(
                "supervisor exited before listening on {}: {status}",
                sock.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("supervisor never listened on {}", sock.display());
}

fn locate_bin(name: &str) -> PathBuf {
    let env_key = format!("CARGO_BIN_EXE_{name}");
    if let Ok(path) = std::env::var(env_key) {
        return PathBuf::from(path);
    }
    let me = std::env::current_exe().expect("current_exe");
    let target_profile = me
        .parent()
        .and_then(|p| p.parent())
        .expect("test bin parent");
    let candidate = target_profile.join(name);
    if candidate.exists() {
        return candidate;
    }
    let status = std::process::Command::new("cargo")
        .args([
            "build",
            "-p",
            "calm-proc-supervisor",
            "--bin",
            "calm-proc-supervisor",
            "--locked",
        ])
        .status()
        .expect("run cargo build for calm-proc-supervisor");
    assert!(
        status.success(),
        "cargo build for calm-proc-supervisor failed with {status}"
    );
    if candidate.exists() {
        return candidate;
    }
    panic!("{name} binary not found at {}", candidate.display());
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .expect("walk up to workspace root")
}
