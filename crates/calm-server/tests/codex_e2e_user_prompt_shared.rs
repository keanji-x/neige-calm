#![cfg(all(unix, feature = "codex-e2e"))]

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::config::Config;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewCove, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use clap::Parser;
use http_body_util::BodyExt;
use serde_json::{Value, json};
// #868: shared no-fallback resolver — env `NEIGE_CODEX_BIN` only, `None` ⇒
// self-skip via `skip!`. Tests must never probe/spawn a PATH codex.
use support::codex_fixture::resolve_codex_bin;
use tower::ServiceExt;

fn cfg(root: &tempfile::TempDir, codex_bin: &Path) -> Config {
    Config::parse_from([
        "calm-server",
        "--data-dir",
        root.path().to_str().unwrap(),
        "--codex-bin",
        codex_bin.to_str().expect("utf-8 codex bin path"),
        // Test codex daemons must NEVER post hooks to the default listen address —
        // that is the production calm-server port on shared boxes (production-kill
        // incident, 2026-07-04); tests do not consume hook ingest.
        "--codex-ingest-url",
        "http://127.0.0.1:1/hooks-disabled-in-e2e",
    ])
}

#[tokio::test]
#[ignore]
async fn user_prompt_card_first_turn_true_binary() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!(
            "codex binary not resolved (NEIGE_CODEX_BIN unset, or not an executable file); CI has no codex"
        );
    };

    let tmp = tempfile::tempdir().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "e2e-user-prompt".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "e2e-user-prompt".into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let cfg = cfg(&tmp, &codex_bin);
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    home.seed().unwrap();
    let shared = SharedCodexAppServer::new(&cfg, Arc::new(home), repo.clone());
    if let Err(e) = shared.start_or_takeover().await {
        eprintln!("skipping: shared app-server did not boot: {e}");
        return;
    }

    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient {
            data_dir: tmp.path().join("terminals"),
            proc_supervisor_sock: None,
        }),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            tmp.path().join("plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    )
    .with_shared_codex_appserver(shared);
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/waves/{}/codex-cards", wave.id))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "cwd": "/tmp",
                        "prompt": "Say OK in one word.",
                        "theme": {"fg": [216,219,226], "bg": [15,20,24]}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let card: Value = serde_json::from_slice(&bytes).unwrap();
    let thread_id = card["payload"]["codex_thread_id"]
        .as_str()
        .expect("codex_thread_id persisted");
    let terminal_id = card["payload"]["terminal_id"]
        .as_str()
        .expect("terminal id");
    let entry = state
        .terminal_renderer
        .get(terminal_id)
        .expect("remote TUI renderer");
    assert!(
        entry.config().args[1].contains(&format!("codex resume '{thread_id}' --remote 'unix://")),
        "remote TUI command should attach the same thread: {}",
        entry.config().args[1]
    );
}
