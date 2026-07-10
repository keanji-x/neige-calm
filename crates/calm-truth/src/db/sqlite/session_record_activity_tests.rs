//! #741 §1.3 — storage-layer coverage for the durable codex
//! worker-liveness feeder `session_record_activity`. Asserts the two
//! push-fed columns land on an active session WITHOUT bumping
//! `updated_at_ms` (worker_sessions-only, like `liveness`), and that a
//! terminal/missing session is a benign `Ok` no-op.
use super::{SqlxRepo, cove_create_tx, session_insert_tx, wave_create_tx};
use crate::model::{NewCove, NewWave, RequestTheme};
use crate::session_repo::SessionRepo;
use calm_types::worker::{
    LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession, WorkerSessionId,
    WorkerSessionState,
};

/// Seed a real cove → wave and insert one worker session in `state` with a
/// fixed `updated_at_ms`. Returns the session id.
async fn seed_session(
    repo: &SqlxRepo,
    session_id: &str,
    state: WorkerSessionState,
    updated_at_ms: i64,
) -> WorkerSessionId {
    seed_session_with_thread(repo, session_id, None, state, updated_at_ms).await
}

/// Like [`seed_session`] but lets the test pin a codex `thread_id` so the
/// thread-keyed feeder path can be exercised.
async fn seed_session_with_thread(
    repo: &SqlxRepo,
    session_id: &str,
    thread_id: Option<&str>,
    state: WorkerSessionState,
    updated_at_ms: i64,
) -> WorkerSessionId {
    let mut tx = repo.pool().begin().await.unwrap();
    let cove = cove_create_tx(
        &mut tx,
        NewCove {
            name: "c".into(),
            color: "#fff".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let wave = wave_create_tx(
        &mut tx,
        NewWave {
            cove_id: cove.id.clone(),
            title: "w".into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        repo.wave_cove_cache(),
    )
    .await
    .unwrap();
    let id = WorkerSessionId::from(session_id);
    let completed_at_ms = state.is_terminal().then_some(updated_at_ms);
    session_insert_tx(
        &mut tx,
        WorkerSession {
            id: id.clone(),
            wave_id: wave.id.clone(),
            provider: WorkerProviderKind::Codex,
            mode: SessionMode::Resumable,
            contract: WorkerContract::Executor,
            parent_session_id: None,
            requester_session_id: None,
            state,
            mcp_token_hash: None,
            thread_id: thread_id.map(str::to_string),
            agent_session_id: None,
            active_turn_id: None,
            terminal_run_id: None,
            card_id: Some(crate::ids::CardId(format!("card-{session_id}"))),
            handle_state_json: None,
            liveness: LivenessTag::Alive,
            liveness_probed_at_ms: None,
            exit_code: None,
            exit_interpretation: None,
            spawn_op_id: None,
            last_activity_ms: None,
            last_thread_status: None,
            created_at_ms: 1,
            updated_at_ms,
            completed_at_ms,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

#[tokio::test]
async fn records_activity_on_active_session_without_bumping_updated_at_ms() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let updated_at_ms = 1_000;
    let id = seed_session(
        &repo,
        "ws-active",
        WorkerSessionState::Running,
        updated_at_ms,
    )
    .await;

    // Pre-condition: both new columns start NULL.
    let before = repo.session_get(&id).await.unwrap().unwrap();
    assert!(before.last_activity_ms.is_none());
    assert!(before.last_thread_status.is_none());

    repo.session_record_activity(&id, 5_000, "active")
        .await
        .unwrap();

    let after = repo.session_get(&id).await.unwrap().unwrap();
    assert_eq!(after.last_activity_ms, Some(5_000));
    assert_eq!(after.last_thread_status.as_deref(), Some("active"));
    // The crux: ws-only columns must NOT touch updated_at_ms (parity).
    assert_eq!(
        after.updated_at_ms, updated_at_ms,
        "session_record_activity must not bump updated_at_ms"
    );
}

#[tokio::test]
async fn record_activity_on_terminal_session_is_benign_noop() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let updated_at_ms = 2_000;
    let id = seed_session(
        &repo,
        "ws-exited",
        WorkerSessionState::Exited,
        updated_at_ms,
    )
    .await;

    // Terminal session: Ok, but no columns change.
    repo.session_record_activity(&id, 9_000, "idle")
        .await
        .unwrap();

    let after = repo.session_get(&id).await.unwrap().unwrap();
    assert!(
        after.last_activity_ms.is_none(),
        "terminal session must not record activity"
    );
    assert!(after.last_thread_status.is_none());
    assert_eq!(after.updated_at_ms, updated_at_ms);
}

#[tokio::test]
async fn record_activity_on_missing_session_is_ok() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let missing = WorkerSessionId::from("ws-nope");
    // Missing row: 0 rows affected is benign and returns Ok.
    repo.session_record_activity(&missing, 7_000, "idle")
        .await
        .unwrap();
    assert!(repo.session_get(&missing).await.unwrap().is_none());
}

#[tokio::test]
async fn records_activity_by_thread_on_active_session_without_bumping_updated_at_ms() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let updated_at_ms = 1_000;
    let id = seed_session_with_thread(
        &repo,
        "ws-active-thread",
        Some("th-active"),
        WorkerSessionState::Running,
        updated_at_ms,
    )
    .await;

    repo.session_record_activity_by_thread("th-active", 5_000, "waitingOnUserInput")
        .await
        .unwrap();

    let after = repo.session_get(&id).await.unwrap().unwrap();
    assert_eq!(after.last_activity_ms, Some(5_000));
    assert_eq!(
        after.last_thread_status.as_deref(),
        Some("waitingOnUserInput")
    );
    // The crux: ws-only columns must NOT touch updated_at_ms (parity).
    assert_eq!(
        after.updated_at_ms, updated_at_ms,
        "session_record_activity_by_thread must not bump updated_at_ms"
    );
}

#[tokio::test]
async fn record_activity_by_thread_on_terminal_session_is_benign_noop() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let updated_at_ms = 2_000;
    let id = seed_session_with_thread(
        &repo,
        "ws-exited-thread",
        Some("th-exited"),
        WorkerSessionState::Exited,
        updated_at_ms,
    )
    .await;

    // Terminal session: Ok, but no columns change.
    repo.session_record_activity_by_thread("th-exited", 9_000, "idle")
        .await
        .unwrap();

    let after = repo.session_get(&id).await.unwrap().unwrap();
    assert!(
        after.last_activity_ms.is_none(),
        "terminal session must not record activity by thread"
    );
    assert!(after.last_thread_status.is_none());
    assert_eq!(after.updated_at_ms, updated_at_ms);
}

#[tokio::test]
async fn record_activity_by_thread_on_unknown_thread_is_ok() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    // Seed an active session under a *different* thread id; the unknown
    // thread must not touch it and must return Ok.
    let id = seed_session_with_thread(
        &repo,
        "ws-other-thread",
        Some("th-known"),
        WorkerSessionState::Running,
        3_000,
    )
    .await;

    repo.session_record_activity_by_thread("th-unknown", 7_000, "active")
        .await
        .unwrap();

    let after = repo.session_get(&id).await.unwrap().unwrap();
    assert!(
        after.last_activity_ms.is_none(),
        "unknown thread must not bleed onto another session"
    );
    assert!(after.last_thread_status.is_none());
}
