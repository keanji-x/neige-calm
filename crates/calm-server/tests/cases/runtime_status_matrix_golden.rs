//! Issue #679 PR0-C — exit-arbitration matrix golden + terminal-absorption
//! characterization.
//!
//! `runtime_status_transition_allowed` (db/sqlite.rs) is the arbitration
//! kernel that every runtime-status writer funnels through today
//! (`session_set_status_tx`, `session_complete_tx`, and their for-card /
//! for-terminal wrappers). The #679 design makes it the future single exit
//! authority, so PR0 pins the CURRENT full decision table as a golden
//! *before* any refactor touches it.
//!
//! Two properties are pinned here:
//!
//!   1. **The full (from × to) matrix** — `goldens/runtime_status_matrix.json`
//!      is a hand-audited 7×7 allow/deny table (14 allow / 35 deny). Every
//!      cell is asserted black-box through the real sqlite write paths
//!      against a real fixture row — the test imports no `fn` internals of
//!      the kernel, only the public `*_tx` writers — so a PR3+ rewrite of
//!      the kernel must reproduce the same observable table to stay green.
//!
//!   2. **Terminal absorption, first writer wins** — two writers racing to
//!      put the same runtime row into a terminal state (the real-world race
//!      between attach_reader EOF, the boot scan, and the terminal sweeper):
//!      the first terminal write lands; the second resolves through the
//!      for-card / for-terminal lookup, finds no *active* runtime, and
//!      no-ops with `Ok(())` — no error, no row mutation. The direct by-id
//!      path is different and also pinned: it surfaces
//!      `IllegalStatusTransition` instead of absorbing silently.
//!
//! Determinism note: sqlite has a single writer lock, so any real
//! concurrent race linearizes into one of exactly two commit orders. We
//! drive both linearizations explicitly (Exited-then-Failed and
//! Failed-then-Exited) with separate committed transactions per writer —
//! same effective schedule as a live race, with zero scheduler flake.
//!
//! Golden discipline: any diff to `goldens/runtime_status_matrix.json` is a
//! semantic change to exit arbitration and needs explicit review under #679.

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_with_codex_create_tx, session_complete_for_card_tx,
    session_complete_for_terminal_tx, session_complete_tx, session_projection_active_for_card_tx,
    session_set_status_for_card_tx, session_set_status_tx, session_start_runtime_tx,
};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::session_projection_repo::{
    WorkerSessionInit, WorkerSessionKind, WorkerSessionProjectionRepoError, WorkerSessionState,
};
use serde_json::{Value, json};

const GOLDEN: &str = include_str!("../goldens/runtime_status_matrix.json");

/// All `WorkerSessionState` variants paired with their pinned db string (the
/// `runtimes.status` CHECK vocabulary from migration 0028). Order matters:
/// it must match the golden's `statuses` array, so adding a variant without
/// updating the golden fails loudly.
const ALL_STATUSES: [(&str, WorkerSessionState); 7] = [
    ("starting", WorkerSessionState::Starting),
    ("running", WorkerSessionState::Running),
    ("idle", WorkerSessionState::Idle),
    ("turn_pending", WorkerSessionState::TurnPending),
    ("failed", WorkerSessionState::Failed),
    ("exited", WorkerSessionState::Exited),
    ("superseded", WorkerSessionState::Superseded),
];

async fn fresh_repo() -> SqlxRepo {
    SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open in-memory sqlite repo")
}

async fn make_wave(repo: &SqlxRepo) -> calm_server::model::Wave {
    let cove = repo
        .cove_create(NewCove {
            name: "exit-matrix".into(),
            color: "#101010".into(),
            sort: None,
        })
        .await
        .expect("create cove");
    repo.wave_create(NewWave {
        workflow_input: None,
        cove_id: cove.id,
        title: "exit matrix".into(),
        sort: None,
        cwd: String::new(),
        workflow_id: None,
        attach_folder: false,
        theme: calm_server::routes::theme::RequestTheme::default_dark(),
    })
    .await
    .expect("create wave")
}

fn terminal_runtime_init(card_id: String, status: WorkerSessionState) -> WorkerSessionInit {
    WorkerSessionInit {
        id: new_id(),
        card_id,
        kind: WorkerSessionKind::Terminal,
        agent_provider: None,
        status,
        terminal_run_id: None,
        thread_id: None,
        session_id: None,
        active_turn_id: None,
        handle_state_json: None,
        spawn_op_id: None,
        now_ms: now_ms(),
    }
}

async fn raw_status(repo: &SqlxRepo, runtime_id: &str) -> String {
    sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id = ?1")
        .bind(runtime_id)
        .fetch_one(repo.pool())
        .await
        .expect("runtime row status")
}

/// Which real write path carries the probed transition into the kernel.
#[derive(Clone, Copy, Debug)]
enum WriterPath {
    SetStatus,
    Complete,
}

/// Seed one fresh card + one runtime row at `from`, then attempt the
/// transition through the given writer. Returns whether the write was
/// allowed, after asserting the row's raw db status reflects the outcome
/// and that a deny is exactly `IllegalStatusTransition { attempted: to }`.
async fn probe(
    repo: &SqlxRepo,
    wave: &calm_server::model::Wave,
    from: &(&str, WorkerSessionState),
    to: &(&str, WorkerSessionState),
    path: WriterPath,
) -> bool {
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .expect("create probe card");

    let mut tx = repo.pool().begin().await.expect("begin probe tx");
    let runtime =
        session_start_runtime_tx(&mut tx, terminal_runtime_init(card.id.to_string(), from.1))
            .await
            .expect("insert probe runtime");
    // Insert round-trip pins the from-side db string mapping.
    assert_eq!(runtime.status, from.1, "insert round-trip for {}", from.0);

    let res = match path {
        WriterPath::SetStatus => session_set_status_tx(&mut tx, &runtime.id, to.1).await,
        WriterPath::Complete => session_complete_tx(&mut tx, &runtime.id, to.1).await,
    };
    tx.commit().await.expect("commit probe tx");

    match res {
        Ok(()) => {
            // Allowed: the row really moved, and the to-side db string
            // mapping is pinned by reading the raw column back.
            assert_eq!(
                raw_status(repo, &runtime.id).await,
                to.0,
                "allowed transition {} -> {} must persist",
                from.0,
                to.0,
            );
            true
        }
        Err(WorkerSessionProjectionRepoError::IllegalStatusTransition { id, attempted }) => {
            assert_eq!(id, runtime.id, "deny names the probed runtime");
            assert_eq!(attempted, to.1, "deny names the attempted status");
            // Denied: the row must be untouched.
            assert_eq!(
                raw_status(repo, &runtime.id).await,
                from.0,
                "denied transition {} -> {} must leave the row at {}",
                from.0,
                to.0,
                from.0,
            );
            false
        }
        Err(other) => panic!("unexpected error probing {} -> {}: {other:?}", from.0, to.0),
    }
}

// ---------------------------------------------------------------------------
// (1) The full matrix, asserted cell by cell against the golden.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn runtime_status_matrix_matches_golden() {
    let golden: Value = serde_json::from_str(GOLDEN).expect("parse golden json");

    // The golden's status vocabulary must be exactly the WorkerSessionState enum,
    // in pinned order — a new variant cannot slip in unaudited.
    let golden_statuses: Vec<&str> = golden["statuses"]
        .as_array()
        .expect("statuses array")
        .iter()
        .map(|v| v.as_str().expect("status string"))
        .collect();
    let expected_statuses: Vec<&str> = ALL_STATUSES.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        golden_statuses, expected_statuses,
        "golden status vocabulary must match WorkerSessionState exactly"
    );

    // Structural exhaustiveness: 7 from-rows × 7 to-cells, values only
    // "allow"/"deny", and exactly 14 allow cells total.
    let matrix = golden["matrix"].as_object().expect("matrix object");
    assert_eq!(matrix.len(), 7, "matrix must have one row per status");
    let mut allow_count = 0usize;
    for (from_name, _) in ALL_STATUSES.iter() {
        let row = matrix[*from_name].as_object().expect("matrix row object");
        assert_eq!(
            row.len(),
            7,
            "row {from_name} must have one cell per status"
        );
        for (to_name, _) in ALL_STATUSES.iter() {
            match row[*to_name].as_str().expect("cell string") {
                "allow" => allow_count += 1,
                "deny" => {}
                other => panic!("cell {from_name} -> {to_name} has bad value {other:?}"),
            }
        }
    }
    assert_eq!(
        allow_count, 14,
        "arbitration matrix has exactly 14 allow cells"
    );

    // Behavioral assertion: every cell, black-box through the real write
    // paths against a real fixture row.
    let repo = fresh_repo().await;
    let wave = make_wave(&repo).await;

    for from in ALL_STATUSES.iter() {
        for to in ALL_STATUSES.iter() {
            let expected_allow = matrix[from.0][to.0].as_str() == Some("allow");

            // `session_set_status_tx` consults the kernel for every target
            // (its categorical Superseded refusal coincides with the
            // matrix's all-deny superseded column).
            let observed = probe(&repo, &wave, from, to, WriterPath::SetStatus).await;
            assert_eq!(
                observed,
                expected_allow,
                "set_status path: {} -> {} (golden says {})",
                from.0,
                to.0,
                if expected_allow { "allow" } else { "deny" },
            );

            // `session_complete_tx` is the other real writer into the
            // kernel; it only accepts terminal targets, so cross-check the
            // failed/exited columns through it as well — both paths must
            // arbitrate identically.
            if matches!(
                to.1,
                WorkerSessionState::Failed | WorkerSessionState::Exited
            ) {
                let observed_complete = probe(&repo, &wave, from, to, WriterPath::Complete).await;
                assert_eq!(
                    observed_complete, expected_allow,
                    "complete path: {} -> {} must arbitrate like set_status",
                    from.0, to.0,
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// (2) Terminal absorption — first writer wins, second writer no-ops.
//
// The production race: attach_reader EOF, the boot scan, and the terminal
// sweeper all observe the same dying runtime and each tries to write a
// terminal status. sqlite's single writer lock linearizes them; whichever
// commits first wins, and the losers' for-card / for-terminal lookups no
// longer see an *active* runtime, so they return Ok(()) without touching
// the row. We pin both possible linearizations deterministically.
// ---------------------------------------------------------------------------

/// Build a real codex worker card (token + terminal + runtime in one tx,
/// the production mint path), advance its runtime to `running`, and return
/// (repo, card, terminal, runtime_id).
async fn running_codex_fixture() -> (
    SqlxRepo,
    calm_server::model::Card,
    calm_server::model::Terminal,
    calm_server::session_projection_repo::RuntimeId,
) {
    let repo = fresh_repo().await;
    let wave = make_wave(&repo).await;

    let mut tx = repo.pool().begin().await.expect("begin mint tx");
    let (card, term, _token) = card_with_codex_create_tx(
        &mut tx,
        new_id(),
        &new_id(),
        None,
        wave.id,
        None,
        None,
        "/workspace".into(),
        json!({"CODEX_HOME": "/tmp/codex-home"}),
        None,
        None,
        None,
        CardRole::Worker,
        true,
        repo.card_role_cache(),
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .expect("mint codex card");
    // Mid-execution shape: starting -> running.
    session_set_status_for_card_tx(&mut tx, card.id.as_ref(), WorkerSessionState::Running)
        .await
        .expect("advance to running");
    let runtime_id = session_projection_active_for_card_tx(&mut tx, card.id.as_ref())
        .await
        .expect("lookup active runtime")
        .expect("active runtime present")
        .id;
    tx.commit().await.expect("commit mint tx");

    (repo, card, term, runtime_id)
}

async fn row_snapshot(repo: &SqlxRepo, runtime_id: &str) -> (String, i64, Option<i64>) {
    sqlx::query_as(
        r#"SELECT state, updated_at_ms, completed_at_ms
           FROM worker_sessions
           WHERE id = ?1"#,
    )
    .bind(runtime_id)
    .fetch_one(repo.pool())
    .await
    .expect("runtime row snapshot")
}

#[tokio::test]
async fn terminal_absorption_exited_first_then_failed_noops() {
    let (repo, card, term, runtime_id) = running_codex_fixture().await;

    // Writer 1 (e.g. attach_reader EOF) — its own tx, commits first, wins.
    let mut tx = repo.pool().begin().await.unwrap();
    session_complete_for_card_tx(&mut tx, card.id.as_ref(), WorkerSessionState::Exited)
        .await
        .expect("first terminal writer succeeds");
    tx.commit().await.unwrap();
    let won = row_snapshot(&repo, &runtime_id).await;
    assert_eq!(won.0, "exited");
    assert!(won.2.is_some(), "first writer stamps completed_at_ms");

    // Writer 2 (e.g. terminal sweeper, via the terminal row) — separate tx,
    // sees no active runtime, no-ops with Ok. No error, no mutation.
    let mut tx = repo.pool().begin().await.unwrap();
    session_complete_for_terminal_tx(&mut tx, &term.id, WorkerSessionState::Failed)
        .await
        .expect("second terminal writer must no-op, not error");
    tx.commit().await.unwrap();
    assert_eq!(
        row_snapshot(&repo, &runtime_id).await,
        won,
        "second writer must not touch the row (status/updated_at/completed_at)"
    );

    // Writer 3 (e.g. boot scan, by card) — same absorption through the
    // for-card wrapper.
    let mut tx = repo.pool().begin().await.unwrap();
    session_complete_for_card_tx(&mut tx, card.id.as_ref(), WorkerSessionState::Failed)
        .await
        .expect("third writer (for-card) must no-op, not error");
    tx.commit().await.unwrap();
    assert_eq!(row_snapshot(&repo, &runtime_id).await, won);

    // Contrast pin: the direct by-id path does NOT absorb — a second
    // terminal write against a known runtime id surfaces the conflict.
    let mut tx = repo.pool().begin().await.unwrap();
    let err = session_complete_tx(&mut tx, &runtime_id, WorkerSessionState::Failed)
        .await
        .expect_err("by-id second terminal write surfaces the conflict");
    drop(tx); // roll back
    assert!(
        matches!(
            err,
            WorkerSessionProjectionRepoError::IllegalStatusTransition {
                attempted: WorkerSessionState::Failed,
                ..
            }
        ),
        "by-id conflict is IllegalStatusTransition, got {err:?}"
    );
    assert_eq!(row_snapshot(&repo, &runtime_id).await, won);
}

#[tokio::test]
async fn terminal_absorption_failed_first_then_exited_noops() {
    // The mirrored linearization of the same race: with sqlite's single
    // writer lock there are exactly two commit orders; this pins the other.
    let (repo, card, term, runtime_id) = running_codex_fixture().await;

    let mut tx = repo.pool().begin().await.unwrap();
    session_complete_for_terminal_tx(&mut tx, &term.id, WorkerSessionState::Failed)
        .await
        .expect("first terminal writer succeeds");
    tx.commit().await.unwrap();
    let won = row_snapshot(&repo, &runtime_id).await;
    assert_eq!(won.0, "failed");
    assert!(won.2.is_some(), "first writer stamps completed_at_ms");

    let mut tx = repo.pool().begin().await.unwrap();
    session_complete_for_card_tx(&mut tx, card.id.as_ref(), WorkerSessionState::Exited)
        .await
        .expect("second terminal writer must no-op, not error");
    tx.commit().await.unwrap();
    assert_eq!(
        row_snapshot(&repo, &runtime_id).await,
        won,
        "failed is absorbed; a later exited cannot overwrite it"
    );
}
