//! #2493 S1: the binding fact, its one writer and the authority derived from it, against the real
//! schema (migration 0161) with foreign keys on.
use calm_types::worker::WorkerSessionState;

use super::workspace_lease_lookup_tests::seed_track;
use super::{
    SqlxRepo, WorkerBinding, WorkerOf, bind_attempt_tx, card_binding_tx, session_binding_tx,
    worker_binding_tx,
};
use crate::model::TaskStatus;

const ALL_SESSION_STATES: [WorkerSessionState; 7] = [
    WorkerSessionState::Starting,
    WorkerSessionState::Running,
    WorkerSessionState::Idle,
    WorkerSessionState::TurnPending,
    WorkerSessionState::Exited,
    WorkerSessionState::Failed,
    WorkerSessionState::Superseded,
];

/// Every variant is listed above: a new state fails to compile here until it is.
fn listed(state: WorkerSessionState) -> bool {
    match state {
        WorkerSessionState::Starting
        | WorkerSessionState::Running
        | WorkerSessionState::Idle
        | WorkerSessionState::TurnPending
        | WorkerSessionState::Exited
        | WorkerSessionState::Failed
        | WorkerSessionState::Superseded => ALL_SESSION_STATES.contains(&state),
    }
}

pub(super) async fn insert_session(
    repo: &SqlxRepo,
    track_id: &str,
    session_id: &str,
    card_id: &str,
    state: WorkerSessionState,
) {
    sqlx::query(
        "INSERT INTO worker_sessions (id, track_id, provider, mode, contract, state, card_id, \
         created_at_ms, updated_at_ms) VALUES (?1, ?2, 'codex', 'ephemeral', 'executor', ?3, ?4, 1, 1)",
    )
    .bind(session_id)
    .bind(track_id)
    .bind(state.as_db_str())
    .bind(card_id)
    .execute(repo.pool())
    .await
    .expect("insert worker session");
}

pub(super) async fn insert_task(repo: &SqlxRepo, track_id: &str, key: &str, status: TaskStatus) {
    sqlx::query(
        "INSERT INTO tasks (id, track_id, key, kind, goal, context_json, depends_on_json, status, \
         declared_by, spawn, created_at_ms, updated_at_ms) \
         VALUES (?1, ?2, ?3, 'codex', 'goal', 'null', '[]', ?4, 'spec', 'in-wave', 1, 1)",
    )
    .bind(format!("{track_id}:{key}"))
    .bind(track_id)
    .bind(key)
    .bind(status.wire_label())
    .execute(repo.pool())
    .await
    .expect("insert task");
}

async fn set_session_state(repo: &SqlxRepo, session_id: &str, state: WorkerSessionState) {
    sqlx::query("UPDATE worker_sessions SET state = ?1 WHERE id = ?2")
        .bind(state.as_db_str())
        .bind(session_id)
        .execute(repo.pool())
        .await
        .expect("set session state");
}

async fn set_task_status(repo: &SqlxRepo, attempt_id: &str, status: TaskStatus) {
    sqlx::query("UPDATE tasks SET status = ?1 WHERE id = ?2")
        .bind(status.wire_label())
        .bind(attempt_id)
        .execute(repo.pool())
        .await
        .expect("set task status");
}

async fn bind(repo: &SqlxRepo, attempt_id: &str, session_id: &str, card_id: &str) -> String {
    let mut conn = repo.pool().acquire().await.expect("acquire");
    match bind_attempt_tx(&mut conn, attempt_id, session_id, card_id).await {
        Ok(()) => "bound".into(),
        Err(error) => error.to_string(),
    }
}

async fn authority(repo: &SqlxRepo, of: WorkerOf<'_>) -> WorkerBinding {
    let mut conn = repo.pool().acquire().await.expect("acquire");
    worker_binding_tx(&mut conn, of).await.expect("authority")
}

#[tokio::test]
async fn session_binding_view_matches_is_active_authority() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let track = seed_track(&repo).await;
    for (index, state) in ALL_SESSION_STATES.into_iter().enumerate() {
        assert!(listed(state));
        let session = format!("session-{index}");
        insert_session(&repo, &track, &session, &format!("card-{index}"), state).await;
        let mut conn = repo.pool().acquire().await.expect("acquire");
        let row = session_binding_tx(&mut conn, &session)
            .await
            .expect("read view")
            .expect("view row");
        assert_eq!(
            row.session_active,
            state.is_active_authority(),
            "the view's `session_active` disagrees with is_active_authority for {state:?}"
        );
    }
}

#[tokio::test]
async fn bind_refuses_non_dispatched() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let track = seed_track(&repo).await;
    for (index, status) in [
        TaskStatus::Pending,
        TaskStatus::Running,
        TaskStatus::Verifying,
        TaskStatus::Done,
        TaskStatus::Failed,
        TaskStatus::Canceled,
    ]
    .into_iter()
    .enumerate()
    {
        let key = format!("k{index}");
        insert_task(&repo, &track, &key, status).await;
        let session = format!("s{index}");
        insert_session(
            &repo,
            &track,
            &session,
            &format!("c{index}"),
            WorkerSessionState::Starting,
        )
        .await;
        let attempt = format!("{track}:{key}");
        let outcome = bind(&repo, &attempt, &session, &format!("c{index}")).await;
        assert!(
            outcome.contains("is not a dispatched attempt without a worker"),
            "a {status:?} attempt must not bind: {outcome}"
        );
        let mut conn = repo.pool().acquire().await.expect("acquire");
        let row = session_binding_tx(&mut conn, &session)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.attempt_id, None, "the refused bind wrote nothing");
    }
    insert_task(&repo, &track, "ready", TaskStatus::Dispatched).await;
    insert_session(
        &repo,
        &track,
        "s-ready",
        "c-ready",
        WorkerSessionState::Starting,
    )
    .await;
    let attempt = format!("{track}:ready");
    assert_eq!(bind(&repo, &attempt, "s-ready", "c-ready").await, "bound");
    let card: Option<String> = sqlx::query_scalar("SELECT worker_card_id FROM tasks WHERE id = ?1")
        .bind(&attempt)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(card.as_deref(), Some("c-ready"), "the bind stamps the card");
    assert!(
        bind(&repo, &attempt, "s-ready", "c-ready")
            .await
            .contains("is not a dispatched attempt without a worker"),
        "an attempt binds once"
    );
}

#[tokio::test]
async fn one_attempt_per_session_is_enforced_by_sql() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let track = seed_track(&repo).await;
    insert_task(&repo, &track, "first", TaskStatus::Done).await;
    insert_task(&repo, &track, "second", TaskStatus::Dispatched).await;
    insert_session(&repo, &track, "s", "c", WorkerSessionState::Idle).await;
    // A writer that skips the guarded bind still cannot give one session two attempts.
    sqlx::query("UPDATE tasks SET worker_session_id = 's' WHERE key = 'first'")
        .execute(repo.pool())
        .await
        .expect("first binding");
    let second = sqlx::query("UPDATE tasks SET worker_session_id = 's' WHERE key = 'second'")
        .execute(repo.pool())
        .await;
    assert!(
        matches!(&second, Err(sqlx::Error::Database(db)) if db.is_unique_violation()),
        "the unique index refuses a second attempt on one session: {second:?}"
    );
    let outcome = bind(&repo, &format!("{track}:second"), "s", "c").await;
    assert!(
        outcome.contains("already serves another attempt"),
        "the writer maps the violation to a Conflict: {outcome}"
    );
}

#[tokio::test]
async fn authority_follows_session_liveness() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let track = seed_track(&repo).await;
    insert_task(&repo, &track, "k", TaskStatus::Dispatched).await;
    insert_session(&repo, &track, "s", "c", WorkerSessionState::Starting).await;
    insert_session(
        &repo,
        &track,
        "plain",
        "c-plain",
        WorkerSessionState::Running,
    )
    .await;
    let attempt = format!("{track}:k");
    assert_eq!(bind(&repo, &attempt, "s", "c").await, "bound");

    let live = |status| WorkerBinding::Live {
        attempt_id: attempt.clone(),
        session_id: "s".into(),
        status,
    };
    for of in [WorkerOf::Session("s"), WorkerOf::Attempt(&attempt)] {
        assert_eq!(authority(&repo, of).await, live(TaskStatus::Dispatched));
    }
    set_task_status(&repo, &attempt, TaskStatus::Running).await;
    set_session_state(&repo, "s", WorkerSessionState::Running).await;
    assert_eq!(
        authority(&repo, WorkerOf::Session("s")).await,
        live(TaskStatus::Running)
    );
    set_task_status(&repo, &attempt, TaskStatus::Verifying).await;
    for of in [WorkerOf::Session("s"), WorkerOf::Attempt(&attempt)] {
        assert_eq!(
            authority(&repo, of).await,
            WorkerBinding::Parked {
                last_attempt_id: attempt.clone(),
                session_id: "s".into()
            }
        );
    }
    set_task_status(&repo, &attempt, TaskStatus::Running).await;
    for ended in [
        WorkerSessionState::Exited,
        WorkerSessionState::Failed,
        WorkerSessionState::Superseded,
    ] {
        set_session_state(&repo, "s", ended).await;
        for of in [WorkerOf::Session("s"), WorkerOf::Attempt(&attempt)] {
            assert_eq!(
                authority(&repo, of).await,
                WorkerBinding::NoSession,
                "an {ended:?} session holds no authority for its running attempt"
            );
        }
        // History still names the attempt.
        let mut conn = repo.pool().acquire().await.expect("acquire");
        let row = card_binding_tx(&mut conn, "c").await.unwrap().unwrap();
        assert_eq!(row.attempt_id.as_deref(), Some(attempt.as_str()));
        assert!(!row.session_active);
    }
    assert_eq!(
        authority(&repo, WorkerOf::Session("plain")).await,
        WorkerBinding::Unbound {
            session_id: "plain".into()
        }
    );
    assert_eq!(
        authority(&repo, WorkerOf::Session("unknown")).await,
        WorkerBinding::NoSession
    );
    assert_eq!(
        authority(&repo, WorkerOf::Attempt("never-bound")).await,
        WorkerBinding::NoSession
    );
}

/// The report flips' guard (`worker_session_id = ?`, #2493): a flip names the session it reports
/// for, and only the session bound to the attempt moves it; the kernel's flip bypasses the guard.
#[tokio::test]
async fn worker_flips_match_only_the_bound_session() {
    use super::{
        TaskReporter, task_complete_from_worker_tx, task_fail_from_worker_tx,
        task_start_verifying_from_worker_tx,
    };
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let track = seed_track(&repo).await;
    insert_session(&repo, &track, "own", "c-own", WorkerSessionState::Running).await;
    insert_session(
        &repo,
        &track,
        "other",
        "c-other",
        WorkerSessionState::Running,
    )
    .await;
    let flip = |key: &'static str| {
        let repo = &repo;
        let track = track.clone();
        async move {
            insert_task(repo, &track, key, TaskStatus::Dispatched).await;
            let attempt = format!("{track}:{key}");
            let session = format!("own-{key}");
            insert_session(
                repo,
                &track,
                &session,
                &format!("c-{key}"),
                WorkerSessionState::Running,
            )
            .await;
            assert_eq!(
                bind(repo, &attempt, &session, &format!("c-{key}")).await,
                "bound"
            );
            (attempt, session)
        }
    };
    for (key, gated) in [("complete", false), ("verify", true), ("fail", false)] {
        let (attempt, session) = flip(key).await;
        if gated {
            sqlx::query("UPDATE tasks SET gate_json = '{}' WHERE id = ?1")
                .bind(&attempt)
                .execute(repo.pool())
                .await
                .unwrap();
        }
        for (reporter, expected) in [
            (
                TaskReporter::Session {
                    session_id: "other",
                },
                0,
            ),
            (
                TaskReporter::Session {
                    session_id: &session,
                },
                1,
            ),
        ] {
            let mut tx = repo.pool().begin().await.unwrap();
            let rows = match key {
                "complete" => task_complete_from_worker_tx(&mut tx, &attempt, &track, reporter, 5)
                    .await
                    .unwrap(),
                "verify" => {
                    task_start_verifying_from_worker_tx(&mut tx, &attempt, &track, reporter, 5)
                        .await
                        .unwrap()
                }
                _ => task_fail_from_worker_tx(&mut tx, &attempt, &track, reporter, "x", 5)
                    .await
                    .unwrap(),
            };
            tx.commit().await.unwrap();
            assert_eq!(rows, expected, "{key} flip by {reporter:?}");
        }
    }
    let (attempt, _) = flip("kernel").await;
    let mut tx = repo.pool().begin().await.unwrap();
    assert_eq!(
        task_fail_from_worker_tx(&mut tx, &attempt, &track, TaskReporter::Kernel, "x", 5)
            .await
            .unwrap(),
        1,
        "the kernel's flip owns the row by construction"
    );
    tx.commit().await.unwrap();
}
