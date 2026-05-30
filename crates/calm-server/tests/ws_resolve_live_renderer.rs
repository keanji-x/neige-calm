#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewCard, NewCove, NewTerminal, NewWave, Terminal};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes::theme::RequestTheme;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::terminal_renderer::RendererConfig;
use calm_server::ws::terminal::{
    TestLiveRenderer, resolve_live_renderer_for_test, resolve_live_renderer_from_terminal_for_test,
};
use serde_json::json;
use tempfile::TempDir;

#[tokio::test]
async fn stale_row_no_supervisor_proc_returns_child_exited() {
    let fixture = TestFixture::boot_with_missing_supervisor().await;
    let term = fixture.seed_terminal().await;

    let live = resolve_live_renderer_for_test(&fixture.state, &term.id)
        .await
        .expect("resolve live renderer");

    assert!(matches!(
        live,
        TestLiveRenderer::ChildExited { exit_code: None }
    ));
    assert!(fixture.state.terminal_renderer.get(&term.id).is_none());
}

#[tokio::test]
async fn live_renderer_entry_returned_when_registry_has_entry() {
    let fixture = TestFixture::boot_with_missing_supervisor().await;
    let term = fixture.seed_terminal().await;
    fixture.insert_live_renderer_entry(&term);

    let live = resolve_live_renderer_for_test(&fixture.state, &term.id)
        .await
        .expect("resolve live renderer");

    match live {
        TestLiveRenderer::Alive(entry) => assert_eq!(entry.terminal_id, term.id),
        TestLiveRenderer::ChildExited { exit_code } => {
            panic!("expected live renderer, got child-exited {exit_code:?}");
        }
    }
    assert!(fixture.state.terminal_renderer.get(&term.id).is_some());
}

#[tokio::test]
async fn probe_failure_returns_child_exited_not_panic() {
    let fixture = TestFixture::boot_with_missing_supervisor().await;
    let term = fixture.seed_terminal().await;

    let live = resolve_live_renderer_for_test(&fixture.state, &term.id)
        .await
        .expect("resolve live renderer");

    assert!(matches!(
        live,
        TestLiveRenderer::ChildExited { exit_code: None }
    ));
}

#[tokio::test]
async fn persisted_exit_code_propagates_through_resolve_live_renderer() {
    let fixture = TestFixture::boot_with_missing_supervisor().await;
    let term = fixture.seed_terminal().await;
    fixture
        .state
        .repo
        .terminal_set_exit(&term.id, Some(137), false)
        .await
        .unwrap();
    let term = fixture
        .state
        .repo
        .terminal_get(&term.id)
        .await
        .unwrap()
        .expect("terminal row");

    let live = resolve_live_renderer_from_terminal_for_test(&fixture.state, term)
        .await
        .expect("resolve live renderer");

    assert!(matches!(
        live,
        TestLiveRenderer::ChildExited {
            exit_code: Some(137)
        }
    ));
}

struct TestFixture {
    _tmp: TempDir,
    repo: Arc<dyn Repo>,
    state: AppState,
}

impl TestFixture {
    async fn boot_with_missing_supervisor() -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let control_sock = tmp.path().join("missing-proc-supervisor.sock");
        Self::boot(tmp, Some(control_sock)).await
    }

    async fn boot(tmp: TempDir, proc_supervisor_sock: Option<PathBuf>) -> Self {
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
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            )),
            Arc::new(CodexClient::new_stub()),
            None,
            None,
        );
        Self {
            _tmp: tmp,
            repo,
            state,
        }
    }

    async fn seed_terminal(&self) -> Terminal {
        let cove = self
            .repo
            .cove_create(NewCove {
                name: "ws-resolve-e2e".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .expect("create cove");
        let wave = self
            .repo
            .wave_create(NewWave {
                cove_id: cove.id,
                title: "ws-resolve-e2e".into(),
                sort: None,
                cwd: workspace_root().display().to_string(),
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .expect("create wave");
        let card = self
            .repo
            .card_create(NewCard {
                wave_id: wave.id,
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

    fn insert_live_renderer_entry(&self, term: &Terminal) {
        let supervisor_sock = self
            .state
            .daemon
            .proc_supervisor_sock
            .clone()
            .expect("supervisor sock");
        self.state
            .terminal_renderer
            .insert_test_entry(RendererConfig {
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
            });
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .expect("walk up to workspace root")
}
