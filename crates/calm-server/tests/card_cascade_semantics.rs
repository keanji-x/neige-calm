//! Issue #679 PR0-C — pin the CURRENT delete-card cleanup semantics.
//!
//! Today, deleting a card destroys its card-owned execution identity in the
//! same transaction:
//!
//!   * `card_mcp_tokens.card_id` -> `cards(id)` ON DELETE CASCADE (migration 0010)
//!   * `worker_sessions.card_id` -> explicit DELETE in `card_delete_tx`
//!
//! `card_delete_tx` deletes same-id `worker_sessions` rows before
//! `DELETE FROM cards`; token cleanup remains FK-driven. That means deleting
//! a *view* (the card) silently kills execution *truth* (the worker's MCP
//! credential and its session rows), even while the session is still active.
//!
//! ⚠ This test pins CURRENT cleanup semantics — do not "fix" this test
//! casually. Any future semantic change must flip this file deliberately, in
//! the same PR, as the design's explicit acknowledgement of the behavior
//! change.
//!
//! Coverage:
//!   1. Route layer (`DELETE /api/cards/:id`, same boot shape as
//!      cards_deletable.rs): real codex worker card minted through
//!      `card_with_codex_create_tx` (card + terminal + MCP token + session
//!      in one tx), session still ACTIVE — delete returns 204 and the token
//!      and session rows are gone.
//!   2. Repo layer (`terminal_delete_tx` + `card_delete_tx` in one tx, the
//!      exact statement sequence the route runs): pins FK-driven token cleanup
//!      plus the explicit same-tx worker-session cleanup.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_delete_tx, card_with_codex_create_tx, terminal_delete_tx,
};
use calm_server::event::EventBus;
use calm_server::model::{Card, CardRole, NewCove, NewWave, Terminal, new_id};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::session_projection_repo::WorkerSessionState;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use serde_json::json;
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    repo: Arc<SqlxRepo>,
    wave_id: String,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "cascade-pin".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "cascade pin".into(),
            sort: None,
            cwd: "/workspace".into(),
            workflow_id: None,
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
    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let state = AppState::from_parts(
        repo.clone() as Arc<dyn Repo>,
        events,
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone() as Arc<dyn Repo>,
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-cascade-pin-test"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache.clone()),
        Some(wave_cove_cache.clone()),
    );

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    Boot {
        app,
        repo,
        wave_id: wave.id.to_string(),
        _tmp: tmp,
    }
}

/// Mint a real codex Worker card through the production tx helper: card row,
/// terminal row, `card_mcp_tokens` row, and an ACTIVE `worker_sessions` row, all in
/// one committed transaction.
async fn mint_codex_worker(boot: &Boot) -> (Card, Terminal) {
    let mut tx = boot.repo.pool().begin().await.expect("begin mint tx");
    let (card, term, token) = card_with_codex_create_tx(
        &mut tx,
        new_id(),
        &new_id(),
        None,
        boot.wave_id.clone().into(),
        None,
        "/workspace".into(),
        json!({"CODEX_HOME": "/tmp/codex-home"}),
        None,
        None,
        None,
        CardRole::Worker,
        true, // user-facing worker card: deletable via REST
        boot.repo.card_role_cache(),
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .expect("mint codex worker card");
    tx.commit().await.expect("commit mint tx");
    assert!(
        token.is_some(),
        "Worker-role codex card mints an MCP token in the same tx"
    );
    (card, term)
}

async fn count(repo: &SqlxRepo, sql: &str, bind: &str) -> i64 {
    sqlx::query_scalar(sql)
        .bind(bind)
        .fetch_one(repo.pool())
        .await
        .expect("count query")
}

async fn token_rows(repo: &SqlxRepo, card_id: &str) -> i64 {
    count(
        repo,
        "SELECT COUNT(*) FROM card_mcp_tokens WHERE card_id = ?1",
        card_id,
    )
    .await
}

async fn runtime_rows(repo: &SqlxRepo, card_id: &str) -> i64 {
    count(
        repo,
        "SELECT COUNT(*) FROM worker_sessions WHERE card_id = ?1",
        card_id,
    )
    .await
}

async fn worker_session_rows(repo: &SqlxRepo, runtime_id: &str) -> i64 {
    count(
        repo,
        "SELECT COUNT(*) FROM worker_sessions WHERE id = ?1",
        runtime_id,
    )
    .await
}

/// Precondition shared by both tests: the freshly minted card really carries
/// execution identity — one token row and one still-ACTIVE runtime row.
async fn assert_identity_present(repo: &SqlxRepo, card_id: &str) -> String {
    assert_eq!(token_rows(repo, card_id).await, 1, "token row minted");
    assert_eq!(runtime_rows(repo, card_id).await, 1, "runtime row minted");
    let active = repo
        .session_projection_active_for_card(&card_id.to_string())
        .await
        .unwrap()
        .expect("runtime is ACTIVE at delete time — the cascade kills a live identity");
    assert_eq!(active.status, WorkerSessionState::Starting);
    assert_eq!(
        worker_session_rows(repo, &active.id).await,
        1,
        "worker_sessions row minted"
    );
    active.id
}

// ---------------------------------------------------------------------------
// (1) Route layer: DELETE /api/cards/:id cascades execution identity away.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_card_route_cascades_mcp_token_and_runtime() {
    let boot = boot().await;
    let (card, term) = mint_codex_worker(&boot).await;
    let card_id = card.id.to_string();
    let runtime_id = assert_identity_present(&boot.repo, &card_id).await;

    let resp = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/cards/{card_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "deletable worker card delete returns 204"
    );

    // The view is gone…
    assert!(boot.repo.card_get(&card_id).await.unwrap().is_none());
    // …and so is the terminal row (route deletes it explicitly: RESTRICT FK).
    assert!(
        boot.repo
            .terminal_get_by_card(&card_id)
            .await
            .unwrap()
            .is_none(),
        "terminal row removed by the route's explicit pre-delete"
    );
    // CURRENT semantics under pin: token identity is destroyed with the
    // card by FK CASCADE, and worker-session identity by the explicit
    // same-tx DELETE in card_delete_tx.
    assert_eq!(
        token_rows(&boot.repo, &card_id).await,
        0,
        "card_mcp_tokens row CASCADE-deleted with the card (migration 0010)"
    );
    assert_eq!(
        runtime_rows(&boot.repo, &card_id).await,
        0,
        "worker_sessions row deleted by explicit DELETE in card_delete_tx"
    );
    assert_eq!(
        worker_session_rows(&boot.repo, &runtime_id).await,
        0,
        "worker_sessions row deleted by card_delete_tx before the card cascade"
    );
    // The terminal id no longer resolves a runtime either (the row is gone,
    // not merely detached via the SET NULL terminal_run_id FK).
    assert_eq!(
        count(
            &boot.repo,
            "SELECT COUNT(*) FROM worker_sessions WHERE terminal_run_id = ?1",
            &term.id,
        )
        .await,
        0
    );
}

// ---------------------------------------------------------------------------
// (2) Repo layer: the same statement sequence the route runs, proving the
// destruction is the schema's FK CASCADE, not route-side compensation.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn card_delete_tx_alone_cascades_mcp_token_and_runtime() {
    let boot = boot().await;
    let (card, term) = mint_codex_worker(&boot).await;
    let card_id = card.id.to_string();
    let runtime_id = assert_identity_present(&boot.repo, &card_id).await;

    // The terminal row must go first — terminals.card_id is ON DELETE
    // RESTRICT (migration 0011) — exactly as the route does it.
    let mut tx = boot.repo.pool().begin().await.unwrap();
    terminal_delete_tx(&mut tx, &term.id).await.unwrap();
    card_delete_tx(&mut tx, card.id.as_ref(), boot.repo.card_role_cache())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert!(boot.repo.card_get(&card_id).await.unwrap().is_none());
    assert_eq!(
        token_rows(&boot.repo, &card_id).await,
        0,
        "token row gone with no explicit token delete in the tx: pure FK CASCADE"
    );
    assert_eq!(
        runtime_rows(&boot.repo, &card_id).await,
        0,
        "worker_sessions row gone via card_delete_tx's explicit same-tx cleanup"
    );
    assert_eq!(
        worker_session_rows(&boot.repo, &runtime_id).await,
        0,
        "worker session row gone via card_delete_tx's explicit same-tx cleanup"
    );
}
