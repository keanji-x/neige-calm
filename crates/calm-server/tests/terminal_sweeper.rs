//! Scope C orphan-terminal sweeper tests. Spec: design doc §10.
//!
//! Coverage:
//!
//!   1. **Orphan detection.** A terminal whose card payload doesn't carry
//!      its `terminal_id` is picked up by `terminals_orphaned`; a live
//!      payload-linked terminal is not.
//!   2. **Grace window.** A freshly-created orphan is held back by the
//!      `grace_seconds` parameter; the same orphan, queried with a smaller
//!      grace, surfaces.
//!   3. **Cleanup emits `TerminalDeleted` with `actor = "kernel"`.** Audit
//!      row lands in the `events` table; bus envelope carries the right
//!      variant.
//!   4. **Idempotent against dead daemon / missing socket.** A row whose
//!      `daemon_handle` points at nothing still gets reaped cleanly (no
//!      panic, no error, audit event emitted).
//!   5. **Non-orphans survive sweep cycles.** A card → terminal pair with
//!      a healthy `payload.terminal_id` is never targeted; multiple sweep
//!      calls leave it intact.
//!
//! Daemon-process killing is exercised at the unit level only — the
//! integration tests don't spawn `calm-session-daemon`. The graceful-kill
//! path is tested by aiming `daemon_handle` at a path that doesn't exist
//! (connect fails → fall through), and the SIGTERM path is bypassed by
//! leaving `pid` as `None` on the seeded row. The full end-to-end with a
//! live daemon is left to the broader CI suite (where the binary is
//! available) — these tests verify the sweep / audit invariants in
//! isolation.

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus};
use calm_server::model::{CardPatch, NewCard, NewCove, NewTerminal, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::terminal_sweeper;
use serde_json::json;

/// Build a fresh in-memory `AppState`. Plugin host is empty; daemon /
/// codex are stubs (no real binaries spawned). Returns the concrete
/// `SqlxRepo` alongside the state so tests can `SELECT` directly out of
/// the events table without going through the `Repo` trait surface
/// (matches the helper shape in `tests/sync_engine.rs`).
async fn fresh_state() -> (AppState, Arc<SqlxRepo>) {
    let concrete = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo: Arc<dyn Repo> = concrete.clone();
    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::card_role_cache::CardRoleCache::new(),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
    );
    (state, concrete)
}

/// Seed a cove + wave + terminal-kind card with `payload.terminal_id` set
/// to the just-created terminal's id. Returns the (card_id, terminal_id)
/// pair. This mirrors the steady-state shape after the 3-step
/// terminal-card create completes (see `eventBridge.tsx:60-70`).
async fn seed_linked_pair(state: &AppState) -> (String, String) {
    let cove = state
        .raw_repo()
        .cove_create(NewCove {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = state
        .raw_repo()
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "w".into(),
            sort: None,
            theme: None,
        })
        .await
        .unwrap();
    let card = state
        .raw_repo()
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "terminal".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    let term = state
        .repo
        .terminal_create(NewTerminal {
            card_id: card.id.clone(),
            program: "/bin/true".into(),
            cwd: "/tmp".into(),
            env: json!({}),
        })
        .await
        .unwrap();
    // Patch the card payload to carry the terminal_id — completes the 3-step
    // create. This is what makes the pair "linked"; without it the terminal
    // would be an orphan.
    state
        .raw_repo()
        .card_update(
            card.id.as_str(),
            CardPatch {
                payload: Some(json!({ "terminal_id": term.id })),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    (card.id.to_string(), term.id)
}

/// Unlink a card from its terminal by stripping `payload.terminal_id`.
/// In production this would happen via card deletion + (failed) FK
/// cascade; in tests it's the simplest way to manufacture the orphan
/// condition `terminals_orphaned` looks for.
async fn unlink_card(state: &AppState, card_id: &str) {
    state
        .raw_repo()
        .card_update(
            card_id,
            CardPatch {
                payload: Some(json!({})),
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

/// Backdate the `created_at` of every terminal row to `now - 120 s` so
/// the sweeper's production 60-second grace window treats them as
/// orphans. Returning early before the sweep call avoids the sleep-
/// in-test antipattern.
async fn age_all_terminals_past_grace(concrete: &SqlxRepo) {
    let cutoff = calm_server::model::now_ms() - 120_000;
    sqlx::query("UPDATE terminals SET created_at = ?1")
        .bind(cutoff)
        .execute(concrete.pool())
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// 1. Orphan detection.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn orphan_detection_skips_linked_pair_and_finds_unlinked() {
    let (state, _concrete) = fresh_state().await;
    let (card_id, terminal_id) = seed_linked_pair(&state).await;

    // With the link in place, even with grace=0 there's no orphan.
    let orphans = state.repo.terminals_orphaned(0).await.unwrap();
    assert!(
        orphans.is_empty(),
        "linked terminal must not appear as orphan, got: {orphans:?}"
    );

    // Drop the link by clearing the card payload.
    unlink_card(&state, &card_id).await;

    // grace=-1 makes any row in the past eligible (the cutoff sits in the
    // future, so every row's `created_at` is less than it).
    let orphans = state.repo.terminals_orphaned(-1).await.unwrap();
    assert_eq!(orphans.len(), 1, "expected one orphan");
    assert_eq!(orphans[0].id, terminal_id);
}

// ---------------------------------------------------------------------------
// 2. Grace window.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn grace_window_holds_back_fresh_orphans() {
    let (state, _concrete) = fresh_state().await;
    let (card_id, terminal_id) = seed_linked_pair(&state).await;

    // Unlink → fresh orphan, just-now `created_at`.
    unlink_card(&state, &card_id).await;

    // Big grace window: orphan is "too fresh" — held back.
    let orphans = state.repo.terminals_orphaned(60).await.unwrap();
    assert!(
        orphans.is_empty(),
        "60s grace must hide a just-created orphan, got: {orphans:?}"
    );

    // grace=-1 forces eligibility regardless of wall-clock skew.
    let orphans = state.repo.terminals_orphaned(-1).await.unwrap();
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].id, terminal_id);
}

// ---------------------------------------------------------------------------
// 3. Cleanup emits TerminalDeleted with actor="kernel".
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sweep_emits_terminal_deleted_with_kernel_actor() {
    let (state, concrete) = fresh_state().await;
    let mut sub = state.events.subscribe();
    let (card_id, terminal_id) = seed_linked_pair(&state).await;

    // Unlink → orphan; backdate so the production-grace `sweep` sees it.
    unlink_card(&state, &card_id).await;
    age_all_terminals_past_grace(&concrete).await;
    // Drain envelopes the seed + unlink emitted (card.added, card.updated x2).
    while sub.try_recv().is_ok() {}

    terminal_sweeper::sweep(&state).await.unwrap();
    // Tiny yield so the bus delivery lands at the subscriber.
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Row gone.
    assert!(
        state
            .repo
            .terminal_get(&terminal_id)
            .await
            .unwrap()
            .is_none(),
        "sweeper must remove the terminal row"
    );

    // Bus saw TerminalDeleted with the terminal's id.
    let env = sub
        .try_recv()
        .expect("sweeper must broadcast TerminalDeleted");
    match env.event {
        Event::TerminalDeleted { id, card_id: c } => {
            assert_eq!(id, terminal_id);
            assert_eq!(c.as_str(), card_id);
        }
        other => panic!("expected TerminalDeleted, got {other:?}"),
    }

    // Events row carries actor="kernel".
    let row: (String, String) = sqlx::query_as(
        "SELECT kind, actor FROM events WHERE id = ?1 AND kind = 'terminal.deleted'",
    )
    .bind(env.id)
    .fetch_one(concrete.pool())
    .await
    .unwrap();
    assert_eq!(row.0, "terminal.deleted");
    // PR2 of #136: events.actor stores the typed ActorId JSON form.
    let actor_json: serde_json::Value = serde_json::from_str(&row.1).unwrap();
    assert_eq!(actor_json, serde_json::json!({"kind": "Kernel"}));
}

// ---------------------------------------------------------------------------
// 4. Idempotent against missing daemon / socket.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cleanup_safe_when_daemon_already_dead() {
    let (state, concrete) = fresh_state().await;
    let (card_id, terminal_id) = seed_linked_pair(&state).await;
    // Aim daemon_handle at a non-existent socket so the graceful-Kill path
    // fails immediately. Don't set a pid → SIGTERM path is also a no-op.
    state
        .repo
        .terminal_set_handle(
            &terminal_id,
            Some("/tmp/calm-sweeper-test-nonexistent.sock"),
        )
        .await
        .unwrap();
    unlink_card(&state, &card_id).await;
    age_all_terminals_past_grace(&concrete).await;

    // Sweep should still complete: graceful Kill fails → SIGTERM skipped
    // (no pid) → socket already gone → row delete works.
    terminal_sweeper::sweep(&state).await.unwrap();

    assert!(
        state
            .repo
            .terminal_get(&terminal_id)
            .await
            .unwrap()
            .is_none(),
        "cleanup must remove the row even when the daemon is already gone"
    );
}

#[tokio::test]
async fn cleanup_safe_with_stale_pid() {
    // A pid persisted from a previous boot may point at nothing (process
    // long since exited and pid recycled to an unrelated unix process we
    // must not signal). The sweeper's `send_sigterm` guards >0 only; we
    // pick a high pid that's very unlikely to exist or matter. The
    // SIGTERM call may return ESRCH or EPERM — both are tolerated.
    let (state, concrete) = fresh_state().await;
    let (card_id, terminal_id) = seed_linked_pair(&state).await;
    // Pick a pid that's almost certainly free.
    state
        .repo
        .terminal_set_pid(&terminal_id, Some(2_000_000_000))
        .await
        .unwrap();
    unlink_card(&state, &card_id).await;
    age_all_terminals_past_grace(&concrete).await;

    terminal_sweeper::sweep(&state).await.unwrap();
    assert!(
        state
            .repo
            .terminal_get(&terminal_id)
            .await
            .unwrap()
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// 5. Non-orphans survive sweep cycles.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn linked_pair_survives_multiple_sweeps() {
    let (state, concrete) = fresh_state().await;
    let (_card_id, terminal_id) = seed_linked_pair(&state).await;
    // Even after aging, the linked pair should never surface as an orphan.
    age_all_terminals_past_grace(&concrete).await;

    for _ in 0..3 {
        terminal_sweeper::sweep(&state).await.unwrap();
    }
    assert!(
        state
            .repo
            .terminal_get(&terminal_id)
            .await
            .unwrap()
            .is_some(),
        "live linked pair must not be reaped"
    );
}
