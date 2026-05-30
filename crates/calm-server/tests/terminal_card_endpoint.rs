//! Integration tests for `POST /api/waves/:wave_id/terminal-cards` —
//! the atomic terminal-card endpoint introduced in #13 PR2.
//!
//! Boots a real Axum router (in-memory `SqlxRepo`) + the actual
//! `calm-session-daemon` binary for the happy paths, and points
//! `DaemonClient::session_daemon_bin` at a non-existent path for the
//! "spawn failure but row persisted" case.
//!
//! Test taxonomy:
//!   * `post_terminal_card_atomic_returns_card_with_linked_payload` — 201,
//!     response is a card with `kind == "terminal"` and
//!     `payload.terminal_id` matching the linked terminal row.
//!   * `post_terminal_card_atomic_emits_single_card_added_event` — exactly
//!     one `card.added` on the bus carrying the final payload; zero
//!     `card.updated`.
//!   * `post_terminal_card_atomic_returns_500_on_daemon_spawn_failure_but_persists_row`
//!     — 500 to the client, but the card + terminal rows are in the DB.
//!   * `post_terminal_card_atomic_404_on_unknown_wave` — 404 + no leaked
//!     rows.
//!   * `post_terminal_card_atomic_defaults_program_to_shell` — empty body
//!     stamps `$SHELL` (or `/bin/sh`) onto the terminal row.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{BroadcastEnvelope, Event, EventBus};
use calm_server::model::{NewCove, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
struct Boot {
    app: axum::Router,
    wave_id: String,
    events: EventBus,
    repo: Arc<dyn Repo>,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "endpoint-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "endpoint-test".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::wave_cove_cache::WaveCoveCache::new(),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    Boot {
        app,
        wave_id: wave.id.to_string(),
        events,
        repo,
        _tmp: tmp,
    }
}

async fn boot_happy() -> Boot {
    boot().await
}

async fn boot_with_bad_supervisor(bad_sock: PathBuf) -> Boot {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "endpoint-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "endpoint-test".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: Some(bad_sock),
    });
    let events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::wave_cove_cache::WaveCoveCache::new(),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    Boot {
        app,
        wave_id: wave.id.to_string(),
        events,
        repo,
        _tmp: tmp,
    }
}

async fn post(app: axum::Router, uri: String, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn get(app: axum::Router, uri: String) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn post_terminal_card_atomic_returns_card_with_linked_payload() {
    let boot = boot_happy().await;

    let (status, card) = post(
        boot.app.clone(),
        format!("/api/waves/{}/terminal-cards", boot.wave_id),
        json!({ "program": "/bin/sh", "cwd": "", "env": {}, "sort": 1.0, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");
    assert_eq!(card["kind"], "terminal", "card kind: {card:?}");
    let terminal_id = card["payload"]["terminal_id"]
        .as_str()
        .expect("payload.terminal_id is a string")
        .to_string();
    assert!(
        !terminal_id.is_empty(),
        "payload.terminal_id non-empty: {card:?}"
    );
    // payload schemaVersion is stamped by the kernel-side helper.
    assert!(
        card["payload"]["schemaVersion"].is_number(),
        "payload.schemaVersion present: {card:?}"
    );

    // The linked terminal row is also visible via the GET helper that
    // `useTodayTerminal` uses. Same id round-trip.
    let card_id = card["id"].as_str().unwrap();
    let (gstatus, term) = get(boot.app.clone(), format!("/api/cards/{card_id}/terminal")).await;
    assert_eq!(gstatus, StatusCode::OK, "GET terminal: {term:?}");
    assert_eq!(term["id"], terminal_id, "terminal id mismatch: {term:?}");
}

#[tokio::test]
async fn post_terminal_card_atomic_emits_single_card_added_event() {
    let boot = boot_happy().await;
    let mut rx = boot.events.subscribe();

    let (status, _card) = post(
        boot.app.clone(),
        format!("/api/waves/{}/terminal-cards", boot.wave_id),
        json!({ "program": "/bin/sh", "cwd": "", "env": {}, "sort": 1.0, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // Drain the bus over a short window. We expect EXACTLY ONE card.added
    // (carrying the fully-stamped payload) and ZERO card.updated frames.
    // The old 3-step recipe used to emit one card.added (payload=null)
    // followed by one card.updated (payload={terminal_id}); the atomic
    // endpoint collapses both into a single broadcast.
    let mut added: Vec<BroadcastEnvelope> = Vec::new();
    let mut updated_count = 0usize;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(200);
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline - now;
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(env)) => match &env.event {
                Event::CardAdded(_) => added.push(env),
                Event::CardUpdated(_) => updated_count += 1,
                _ => {}
            },
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    assert_eq!(
        added.len(),
        1,
        "exactly one card.added; got {} added + {} updated",
        added.len(),
        updated_count
    );
    assert_eq!(
        updated_count, 0,
        "no card.updated allowed; got {updated_count}"
    );
    let env = added.into_iter().next().unwrap();
    match env.event {
        Event::CardAdded(card) => {
            assert_eq!(card.kind, "terminal");
            assert!(
                card.payload
                    .get("terminal_id")
                    .and_then(|v| v.as_str())
                    .is_some(),
                "card.added event payload must carry terminal_id: {:?}",
                card.payload
            );
        }
        other => panic!("expected CardAdded, got {other:?}"),
    }
}

#[tokio::test]
async fn post_terminal_card_atomic_returns_500_on_daemon_spawn_failure_but_persists_row() {
    // Point the renderer at a supervisor socket that definitely doesn't exist.
    // The handler must:
    //   (a) propagate the 500 to the caller, AND
    //   (b) NOT roll back the card+terminal txn (the sweeper cleans up).
    let bad_sock = std::env::temp_dir().join("definitely-not-a-real-proc-supervisor.sock");
    let _ = std::fs::remove_file(&bad_sock);
    let boot = boot_with_bad_supervisor(bad_sock).await;

    let (status, body) = post(
        boot.app.clone(),
        format!("/api/waves/{}/terminal-cards", boot.wave_id),
        json!({ "program": "/bin/sh", "cwd": "", "env": {}, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "expected 500 on daemon spawn failure: {body:?}"
    );

    // Even though spawn failed, the card + terminal row must remain in the
    // DB — the orphan-terminal sweeper is responsible for cleanup.
    let cards = boot.repo.cards_by_wave(&boot.wave_id).await.unwrap();
    assert_eq!(
        cards.len(),
        1,
        "exactly one card persisted; got {}",
        cards.len()
    );
    let card = &cards[0];
    assert_eq!(card.kind, "terminal");
    let terminal_id = card.payload["terminal_id"]
        .as_str()
        .expect("payload.terminal_id stamped before spawn attempt");
    let term = boot
        .repo
        .terminal_get(terminal_id)
        .await
        .unwrap()
        .expect("terminal row persisted despite spawn failure");
    assert_eq!(term.card_id, card.id);
}

#[tokio::test]
async fn post_terminal_card_atomic_404_on_unknown_wave() {
    // No daemon spawn happens on the 404 path, so the stub binary is fine.
    let boot = boot().await;

    let (status, body) = post(
        boot.app.clone(),
        "/api/waves/wave_does_not_exist/terminal-cards".to_string(),
        json!({ "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body={body:?}");

    // No card and no terminal row leaked under the original wave either.
    let cards = boot.repo.cards_by_wave(&boot.wave_id).await.unwrap();
    assert!(cards.is_empty(), "no rows must leak on 404: {cards:?}");
}

#[tokio::test]
async fn post_terminal_card_atomic_defaults_program_to_shell() {
    let boot = boot_happy().await;
    let (status, card) = post(
        boot.app.clone(),
        format!("/api/waves/{}/terminal-cards", boot.wave_id),
        // Only required field (#177): theme. Every other field falls
        // back to its default (program → $SHELL, cwd → $HOME, env → {}).
        json!({ "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");
    let terminal_id = card["payload"]["terminal_id"]
        .as_str()
        .expect("payload.terminal_id is a string");
    let term = boot
        .repo
        .terminal_get(terminal_id)
        .await
        .unwrap()
        .expect("terminal row exists after create");
    // `$SHELL` → falls back to `/bin/sh`. We accept either form so the test
    // is robust across host envs (CI typically has no $SHELL exported).
    let expected = std::env::var("SHELL").unwrap_or_default();
    if expected.is_empty() {
        assert_eq!(
            term.program, "/bin/sh",
            "default program: {:?}",
            term.program
        );
    } else {
        assert_eq!(
            term.program, expected,
            "default program should match $SHELL: {:?}",
            term.program
        );
    }
}
