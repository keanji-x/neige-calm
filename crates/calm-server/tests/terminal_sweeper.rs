//! Scope C orphan-terminal sweeper tests. Spec: design doc §10.
//!
//! Coverage:
//!
//!   1. **Orphan detection.** A terminal without an active runtime owner is
//!      picked up by `terminals_orphaned`; live runtime-owned terminals are not.
//!   2. **Grace window.** A freshly-created orphan is held back by the
//!      `grace_seconds` parameter; the same orphan, queried with a smaller
//!      grace, surfaces.
//!   3. **Cleanup emits `TerminalDeleted` with `actor = "kernel"`.** Audit
//!      row lands in the `events` table; bus envelope carries the right
//!      variant.
//!   4. **Idempotent against dead daemon / missing socket.** A row whose
//!      `renderer entry` points at nothing still gets reaped cleanly (no
//!      panic, no error, audit event emitted).
//!   5. **Non-orphans survive sweep cycles.** A terminal with an active
//!      runtime owner is never targeted; multiple sweep calls leave it intact.
//!
//! Daemon-process killing is exercised at the unit level only — the
//! integration tests don't start a terminal renderer. The graceful-kill
//! path is tested by aiming `renderer entry` at a path that doesn't exist
//! (connect fails → fall through), and the SIGTERM path is bypassed by
//! leaving `pid` as `None` on the seeded row. The full end-to-end with a
//! live daemon is left to the broader CI suite (where the binary is
//! available) — these tests verify the sweep / audit invariants in
//! isolation.

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_with_codex_create_tx, runtime_complete_for_card_tx, runtime_start_tx,
};
use calm_server::event::{Event, EventBus};
use calm_server::model::{
    CardPatch, CardRole, NewCard, NewCove, NewTerminal, NewWave, new_id, now_ms,
};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind};
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
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );
    (state, concrete)
}

/// Seed a cove + wave + terminal-kind card with a terminal row and active
/// terminal runtime. Returns the (card_id, terminal_id) pair.
async fn seed_linked_pair(state: &AppState, concrete: &SqlxRepo) -> (String, String) {
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
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
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
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let mut tx = concrete.pool().begin().await.unwrap();
    runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: new_id(),
            card_id: card.id.to_string(),
            kind: RuntimeKind::Terminal,
            agent_provider: None,
            status: RunStatus::Running,
            terminal_run_id: Some(term.id.clone()),
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            lease_owner: None,
            lease_until_ms: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (card.id.to_string(), term.id)
}

/// Seed a spec codex card + terminal whose active runtime is a shared-spec
/// row with `thread_id` bound and `terminal_run_id = NULL`, matching the
/// post-migration shape for bound shared-spec threads.
async fn seed_shared_spec_pair(
    state: &AppState,
    concrete: &SqlxRepo,
    thread_id: &str,
) -> (String, String) {
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
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let mut tx = concrete.pool().begin().await.unwrap();
    let (card, term, _mcp_token) = card_with_codex_create_tx(
        &mut tx,
        new_id(),
        &new_id(),
        None,
        wave.id,
        None,
        "/tmp".into(),
        json!({}),
        None,
        None,
        None,
        CardRole::Spec,
        false,
        concrete.card_role_cache(),
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .unwrap();
    runtime_complete_for_card_tx(&mut tx, card.id.as_ref(), RunStatus::Exited)
        .await
        .unwrap();
    runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: new_id(),
            card_id: card.id.to_string(),
            kind: RuntimeKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Running,
            terminal_run_id: None,
            thread_id: Some(thread_id.into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            lease_owner: None,
            lease_until_ms: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (card.id.to_string(), term.id)
}

async fn seed_migrated_shared_spec_pair(state: &AppState, concrete: &SqlxRepo) -> (String, String) {
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
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let mut tx = concrete.pool().begin().await.unwrap();
    let (card, term, _mcp_token) = card_with_codex_create_tx(
        &mut tx,
        new_id(),
        &new_id(),
        None,
        wave.id,
        None,
        "/tmp".into(),
        json!({}),
        None,
        None,
        None,
        CardRole::Spec,
        false,
        concrete.card_role_cache(),
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .unwrap();
    runtime_complete_for_card_tx(&mut tx, card.id.as_ref(), RunStatus::Exited)
        .await
        .unwrap();
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO runtimes (
               id, card_id, kind, agent_provider, status, terminal_run_id,
               thread_id, session_id, active_turn_id, handle_state_json,
               lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
               completed_at_ms
           )
           VALUES (?1, ?2, 'shared-spec', 'codex', 'running', NULL,
                   't1', NULL, NULL, NULL, NULL, NULL, ?3, ?3, NULL)"#,
    )
    .bind(new_id())
    .bind(card.id.as_str())
    .bind(now)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (card.id.to_string(), term.id)
}

/// Strip any legacy payload link. This should not affect orphan detection;
/// runtime ownership is now the contract.
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

async fn complete_terminal_runtime_for_card(state: &AppState, card_id: &str) {
    let Some(term) = state.repo.terminal_get_by_card(card_id).await.unwrap() else {
        return;
    };
    state
        .repo
        .runtime_complete_for_terminal(&term.id, RunStatus::Exited)
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
async fn orphan_detection_skips_runtime_owned_terminal_and_finds_orphan() {
    let (state, concrete) = fresh_state().await;
    let (card_id, terminal_id) = seed_linked_pair(&state, &concrete).await;

    // With an active runtime owner, even with grace=0 there's no orphan.
    let orphans = state.repo.terminals_orphaned(0).await.unwrap();
    assert!(
        orphans.is_empty(),
        "runtime-owned terminal must not appear as orphan, got: {orphans:?}"
    );

    // Complete the runtime, leaving the terminal without an active owner.
    unlink_card(&state, &card_id).await;
    complete_terminal_runtime_for_card(&state, &card_id).await;

    // grace=-1 makes any row in the past eligible (the cutoff sits in the
    // future, so every row's `created_at` is less than it).
    let orphans = state.repo.terminals_orphaned(-1).await.unwrap();
    assert_eq!(orphans.len(), 1, "expected one orphan");
    assert_eq!(orphans[0].id, terminal_id);
}

#[tokio::test]
async fn orphan_detection_skips_runtime_owned_terminal_without_payload_link() {
    let (state, concrete) = fresh_state().await;
    let (card_id, _terminal_id) = seed_linked_pair(&state, &concrete).await;

    unlink_card(&state, &card_id).await;

    let orphans = state.repo.terminals_orphaned(-1).await.unwrap();
    assert!(
        orphans.is_empty(),
        "active runtime-owned terminal must not appear as orphan, got: {orphans:?}"
    );
}

#[tokio::test]
async fn orphan_sweep_protects_shared_spec_terminal_with_null_terminal_run_id() {
    let (state, concrete) = fresh_state().await;
    let (_card_id, terminal_id) = seed_shared_spec_pair(&state, &concrete, "t1").await;
    age_all_terminals_past_grace(&concrete).await;

    terminal_sweeper::sweep(&state).await.unwrap();

    assert!(
        state
            .repo
            .terminal_get(&terminal_id)
            .await
            .unwrap()
            .is_some(),
        "active shared-spec runtime must protect its card terminal"
    );
}

#[tokio::test]
async fn orphan_sweep_reaps_terminal_after_runtime_completion() {
    let (state, concrete) = fresh_state().await;
    let (card_id, terminal_id) = seed_shared_spec_pair(&state, &concrete, "t1").await;

    state
        .repo
        .runtime_complete_for_card(&card_id, RunStatus::Exited)
        .await
        .unwrap();
    age_all_terminals_past_grace(&concrete).await;

    terminal_sweeper::sweep(&state).await.unwrap();

    assert!(
        state
            .repo
            .terminal_get(&terminal_id)
            .await
            .unwrap()
            .is_none(),
        "completed shared-spec runtime must leave terminal orphan-eligible"
    );
}

#[tokio::test]
async fn migrated_shared_spec_terminal_survives_sweep() {
    let (state, concrete) = fresh_state().await;
    let (_card_id, terminal_id) = seed_migrated_shared_spec_pair(&state, &concrete).await;
    age_all_terminals_past_grace(&concrete).await;

    terminal_sweeper::sweep(&state).await.unwrap();

    assert!(
        state
            .repo
            .terminal_get(&terminal_id)
            .await
            .unwrap()
            .is_some(),
        "migration-shaped active shared-spec runtime must protect terminal"
    );
}

// ---------------------------------------------------------------------------
// 2. Grace window.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn grace_window_holds_back_fresh_orphans() {
    let (state, concrete) = fresh_state().await;
    let (card_id, terminal_id) = seed_linked_pair(&state, &concrete).await;

    // Complete the runtime -> fresh orphan, just-now `created_at`.
    unlink_card(&state, &card_id).await;
    complete_terminal_runtime_for_card(&state, &card_id).await;

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
    let (card_id, terminal_id) = seed_linked_pair(&state, &concrete).await;

    // Complete the runtime -> orphan; backdate so the production-grace `sweep` sees it.
    unlink_card(&state, &card_id).await;
    complete_terminal_runtime_for_card(&state, &card_id).await;
    age_all_terminals_past_grace(&concrete).await;
    // Drain any fixture envelopes before the sweeper emits TerminalDeleted.
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
// 4. Idempotent against missing renderer / pid.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cleanup_safe_when_daemon_already_dead() {
    let (state, concrete) = fresh_state().await;
    let (card_id, terminal_id) = seed_linked_pair(&state, &concrete).await;
    // No renderer entry and no pid: cleanup should still delete the row.
    unlink_card(&state, &card_id).await;
    complete_terminal_runtime_for_card(&state, &card_id).await;
    age_all_terminals_past_grace(&concrete).await;

    // Sweep should still complete: renderer shutdown misses → SIGTERM
    // skipped (no pid) → row delete works.
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
    let (card_id, terminal_id) = seed_linked_pair(&state, &concrete).await;
    // Pick a pid that's almost certainly free.
    state
        .repo
        .terminal_set_pid(&terminal_id, Some(2_000_000_000))
        .await
        .unwrap();
    unlink_card(&state, &card_id).await;
    complete_terminal_runtime_for_card(&state, &card_id).await;
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
async fn runtime_owned_terminal_survives_multiple_sweeps() {
    let (state, concrete) = fresh_state().await;
    let (_card_id, terminal_id) = seed_linked_pair(&state, &concrete).await;
    // Even after aging, the runtime-owned terminal should never surface as an orphan.
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
        "live runtime-owned terminal must not be reaped"
    );
}

// ---------------------------------------------------------------------------
// 6. `reap_terminal_pid_only` (issue #310 followup): pid-only partial-spawn
//    SIGTERM helper.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reap_terminal_pid_only_sigterms_live_pid() {
    // Spawn a long-lived child that ignores nothing — default SIGTERM
    // handling terminates `sleep` immediately. `sleep 300` gives the test
    // plenty of slack before we'd need to fall back to SIGKILL on a leak.
    let mut child = tokio::process::Command::new("sleep")
        .arg("300")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn sleep");
    let pid: i32 = child.id().expect("child pid available") as i32;

    // Sanity check: child is alive before the reap. `try_wait()` is None
    // for a still-running child.
    assert!(
        child.try_wait().expect("try_wait ok").is_none(),
        "fixture child must be alive before reap"
    );

    // Drive the helper. It's best-effort and returns nothing — success is
    // observed by the child exiting.
    terminal_sweeper::reap_terminal_pid_only("test-terminal-id", pid as i64);

    // Poll `try_wait()` (rather than `kill(pid, 0)`) — the parent hasn't
    // reaped the zombie yet, so a `kill(pid, 0)` probe would keep
    // returning 0 even after the child exited. `try_wait()` is the
    // canonical "did my child terminate?" check and reaps in the same
    // call when it has.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut exit_status = None;
    while std::time::Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("try_wait ok") {
            exit_status = Some(status);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let status = exit_status.unwrap_or_else(|| {
        panic!("reap_terminal_pid_only must SIGTERM the supplied pid; child {pid} survived")
    });
    // SIGTERM-killed children report signal-termination, not a clean exit
    // code. `ExitStatus::code()` returns None for signal exits on unix.
    assert!(
        status.code().is_none(),
        "child exited but not via signal; expected SIGTERM termination, got {status:?}",
    );
}

#[tokio::test]
async fn reap_terminal_pid_only_tolerates_dead_pid() {
    // Idempotent against pids that already vanished (the common case when
    // the daemon races us and exits between the row read and the helper
    // call). Pick a pid that's almost certainly unallocated and assert the
    // helper doesn't panic / propagate.
    terminal_sweeper::reap_terminal_pid_only("test-terminal-id", 2_000_000_000);
}
