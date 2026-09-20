//! Exit-arbitration matrix golden (`goldens/runtime_status_matrix.json`) and terminal-absorption
//! characterization for `runtime_status_transition_allowed`. Any diff to the golden is a semantic change.

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_with_codex_create_tx, session_complete_for_card_tx,
    session_complete_for_terminal_tx, session_complete_tx, session_projection_active_for_card_tx,
    session_set_status_for_card_tx, session_set_status_tx, session_start_runtime_tx,
};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack, new_id, now_ms};
use calm_server::session_projection_repo::{
    WorkerSessionInit, WorkerSessionKind, WorkerSessionProjectionRepoError, WorkerSessionState,
};
use serde_json::{Value, json};

const GOLDEN: &str = include_str!("../goldens/runtime_status_matrix.json");

/// Order matters: it must match the golden's `statuses` array, so adding a variant without updating the golden fails loudly.
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

async fn make_track(repo: &SqlxRepo) -> calm_server::model::Track {
    let area = repo
        .area_create(NewArea {
            name: "exit-matrix".into(),
            color: "#101010".into(),
            sort: None,
        })
        .await
        .expect("create area");
    repo.track_create(NewTrack {
        template_input: None,
        area_id: area.id,
        title: "exit matrix".into(),
        sort: None,
        cwd: String::new(),
        template_id: None,
        plugin_scope: None,
        attach_folder: false,
        theme: calm_server::routes::theme::RequestTheme::default_dark(),
    })
    .await
    .expect("create track")
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

/// Seed one fresh card + one runtime row at `from`, then attempt the transition through the given writer.
async fn probe(
    repo: &SqlxRepo,
    track: &calm_server::model::Track,
    from: &(&str, WorkerSessionState),
    to: &(&str, WorkerSessionState),
    path: WriterPath,
) -> bool {
    let card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
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
    assert_eq!(runtime.status, from.1, "insert round-trip for {}", from.0);

    let res = match path {
        WriterPath::SetStatus => session_set_status_tx(&mut tx, &runtime.id, to.1).await,
        WriterPath::Complete => session_complete_tx(&mut tx, &runtime.id, to.1).await,
    };
    tx.commit().await.expect("commit probe tx");

    match res {
        Ok(()) => {
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

#[tokio::test]
async fn runtime_status_matrix_matches_golden() {
    let golden: Value = serde_json::from_str(GOLDEN).expect("parse golden json");

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

    let repo = fresh_repo().await;
    let track = make_track(&repo).await;

    for from in ALL_STATUSES.iter() {
        for to in ALL_STATUSES.iter() {
            let expected_allow = matrix[from.0][to.0].as_str() == Some("allow");

            // `session_set_status_tx`'s categorical Superseded refusal coincides with the matrix's all-deny superseded column.
            let observed = probe(&repo, &track, from, to, WriterPath::SetStatus).await;
            assert_eq!(
                observed,
                expected_allow,
                "set_status path: {} -> {} (golden says {})",
                from.0,
                to.0,
                if expected_allow { "allow" } else { "deny" },
            );

            // `session_complete_tx` only accepts terminal targets; both paths must arbitrate identically.
            if matches!(
                to.1,
                WorkerSessionState::Failed | WorkerSessionState::Exited
            ) {
                let observed_complete = probe(&repo, &track, from, to, WriterPath::Complete).await;
                assert_eq!(
                    observed_complete, expected_allow,
                    "complete path: {} -> {} must arbitrate like set_status",
                    from.0, to.0,
                );
            }
        }
    }
}

// sqlite's single writer lock linearizes racing terminal writers; the losers' for-card / for-terminal
// lookups no longer see an *active* runtime and return Ok(()) without touching the row.

async fn running_codex_fixture() -> (
    SqlxRepo,
    calm_server::model::Card,
    calm_server::model::Terminal,
    String,
) {
    let repo = fresh_repo().await;
    let track = make_track(&repo).await;

    let mut tx = repo.pool().begin().await.expect("begin mint tx");
    let (card, term, _token) = card_with_codex_create_tx(
        &mut tx,
        new_id(),
        &new_id(),
        None,
        track.id,
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

    let mut tx = repo.pool().begin().await.unwrap();
    session_complete_for_card_tx(&mut tx, card.id.as_ref(), WorkerSessionState::Exited)
        .await
        .expect("first terminal writer succeeds");
    tx.commit().await.unwrap();
    let won = row_snapshot(&repo, &runtime_id).await;
    assert_eq!(won.0, "exited");
    assert!(won.2.is_some(), "first writer stamps completed_at_ms");

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

    let mut tx = repo.pool().begin().await.unwrap();
    session_complete_for_card_tx(&mut tx, card.id.as_ref(), WorkerSessionState::Failed)
        .await
        .expect("third writer (for-card) must no-op, not error");
    tx.commit().await.unwrap();
    assert_eq!(row_snapshot(&repo, &runtime_id).await, won);

    // The direct by-id path does NOT absorb.
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
    // The mirrored linearization: with sqlite's single writer lock there are exactly two commit orders.
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
