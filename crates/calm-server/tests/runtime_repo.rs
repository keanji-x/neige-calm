mod support;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_with_claude_create_tx, card_with_codex_create_tx, card_with_terminal_create_tx,
    session_bind_attribution_tx, session_commit_exit_tx, session_complete_for_card_tx,
    session_complete_tx, session_fail_if_active_runtime_tx, session_insert_tx,
    session_mark_superseded_runtime_tx, session_mcp_token_set_tx, session_prepare_deferred_spec_tx,
    session_projection_active_for_card_tx, session_projection_by_id_tx,
    session_restore_from_superseded_runtime_tx, session_set_active_turn_tx,
    session_set_handle_state_tx, session_set_harness_observation_runtime_tx,
    session_set_status_for_card_tx, session_set_status_tx, session_start_runtime_tx,
    session_supersede_and_start_tx,
};
use calm_server::ids::CardId;
use calm_server::model::{Card, CardRole, NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::session_projection_lookup::project_runtime_into_card_payload;
use calm_server::session_projection_repo::{
    AgentProvider, ThreadAttribution, WorkerSessionInit, WorkerSessionKind,
    WorkerSessionProjectionRepo, WorkerSessionProjectionRepoError, WorkerSessionState,
};
use calm_types::worker::{
    ExitInterpretation, LivenessTag, SessionMode, WorkerContract, WorkerProviderKind,
    WorkerSession, WorkerSessionId,
};
use serde_json::json;

async fn fresh_repo() -> SqlxRepo {
    SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open in-memory sqlite repo")
}

async fn make_wave(repo: &SqlxRepo) -> calm_server::model::Wave {
    let cove = repo
        .cove_create(NewCove {
            name: "runtime-repo".into(),
            color: "#101010".into(),
            sort: None,
        })
        .await
        .expect("create cove");
    repo.wave_create(NewWave {
        cove_id: cove.id,
        title: "runtime repo".into(),
        sort: None,
        cwd: String::new(),
        workflow_id: None,
        attach_folder: false,
        theme: calm_server::routes::theme::RequestTheme::default_dark(),
    })
    .await
    .expect("create wave")
}

async fn make_card(repo: &SqlxRepo, kind: &str) -> Card {
    let wave = make_wave(repo).await;
    make_card_in_wave(repo, wave.id, kind).await
}

async fn make_card_in_wave(repo: &SqlxRepo, wave_id: calm_server::ids::WaveId, kind: &str) -> Card {
    repo.card_create(NewCard {
        wave_id,
        kind: kind.into(),
        sort: None,
        payload: json!({"schemaVersion": 1}),
    })
    .await
    .expect("create card")
}

fn runtime_init(
    card_id: String,
    kind: WorkerSessionKind,
    agent_provider: Option<AgentProvider>,
    status: WorkerSessionState,
) -> WorkerSessionInit {
    WorkerSessionInit {
        id: new_id(),
        card_id,
        kind,
        agent_provider,
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

async fn runtime_row_snapshot(repo: &SqlxRepo, runtime_id: &str) -> (String, i64, Option<i64>) {
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

async fn runtime_by_id_tx_snapshot(
    repo: &SqlxRepo,
    runtime_id: &str,
) -> Option<calm_server::session_projection_repo::WorkerSessionProjection> {
    let id = runtime_id.to_string();
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_projection_by_id_tx(&mut tx, &id).await.unwrap();
    tx.commit().await.unwrap();
    runtime
}

async fn mint_terminal_session(
    repo: &SqlxRepo,
    spawn_op_id: Option<&str>,
) -> (WorkerSessionId, calm_server::ids::WaveId) {
    if let Some(op_id) = spawn_op_id {
        ensure_test_operation(repo, op_id, "terminal-worker", op_id).await;
    }
    let wave = make_wave(repo).await;
    let runtime_id = new_id();
    let mut tx = repo.pool().begin().await.unwrap();
    card_with_terminal_create_tx(
        &mut tx,
        new_id(),
        &runtime_id,
        spawn_op_id,
        wave.id.clone(),
        None,
        "bash".into(),
        "/tmp".into(),
        json!({}),
        CardRole::Worker,
        true,
        repo.card_role_cache(),
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (WorkerSessionId(runtime_id), wave.id)
}

async fn mint_codex_session(
    repo: &SqlxRepo,
    spawn_op_id: Option<&str>,
) -> (WorkerSessionId, calm_server::ids::WaveId) {
    if let Some(op_id) = spawn_op_id {
        ensure_test_operation(repo, op_id, "codex-worker", op_id).await;
    }
    let wave = make_wave(repo).await;
    let runtime_id = new_id();
    let mut tx = repo.pool().begin().await.unwrap();
    card_with_codex_create_tx(
        &mut tx,
        new_id(),
        &runtime_id,
        spawn_op_id,
        wave.id.clone(),
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
    .unwrap();
    tx.commit().await.unwrap();
    (WorkerSessionId(runtime_id), wave.id)
}

async fn ensure_test_operation(repo: &SqlxRepo, op_id: &str, kind: &str, idempotency_key: &str) {
    let now = now_ms();
    let target_json = serde_json::to_string(&json!({"type": "wave"})).unwrap();
    let payload_json = serde_json::to_string(&json!({
        "idempotency_key": idempotency_key
    }))
    .unwrap();
    sqlx::query(
        r#"INSERT OR IGNORE INTO operations (
               id, operation_key, kind, idempotency_key, payload_hash,
               target_type, target_id, target_json, payload_json,
               phase, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, ?3, ?4, ?5,
                   'wave', NULL, ?6, ?7,
                   'pending', ?8, ?8)"#,
    )
    .bind(op_id)
    .bind(new_id())
    .bind(kind)
    .bind(idempotency_key)
    .bind(format!("{kind}:{idempotency_key}"))
    .bind(target_json)
    .bind(payload_json)
    .bind(now)
    .execute(repo.pool())
    .await
    .unwrap();
}

fn worker_session(
    id: &str,
    wave_id: calm_server::ids::WaveId,
    state: WorkerSessionState,
    hash: &str,
) -> WorkerSession {
    let now = now_ms();
    WorkerSession {
        id: WorkerSessionId::from(id),
        wave_id,
        provider: WorkerProviderKind::Codex,
        mode: SessionMode::Resumable,
        contract: WorkerContract::Planner,
        parent_session_id: None,
        requester_session_id: None,
        state,
        mcp_token_hash: Some(hash.to_string()),
        thread_id: Some(format!("thread-{id}")),
        agent_session_id: None,
        active_turn_id: None,
        terminal_run_id: None,
        card_id: Some(CardId(format!("card-{id}"))),
        handle_state_json: Some(json!({"mode": "harness"})),
        liveness: LivenessTag::Unknown,
        liveness_probed_at_ms: None,
        exit_code: None,
        exit_interpretation: None,
        spawn_op_id: None,
        last_activity_ms: None,
        last_thread_status: None,
        created_at_ms: now,
        updated_at_ms: now,
        completed_at_ms: state.is_terminal().then_some(now),
    }
}

#[tokio::test]
async fn session_get_by_active_token_hash_filters_terminal_rows() {
    let repo = fresh_repo().await;
    let wave = make_wave(&repo).await;
    let rows = [
        (
            "ws-active-starting",
            WorkerSessionState::Starting,
            "hash-active-starting",
            true,
        ),
        (
            "ws-active-running",
            WorkerSessionState::Running,
            "hash-active-running",
            true,
        ),
        (
            "ws-active-idle",
            WorkerSessionState::Idle,
            "hash-active-idle",
            true,
        ),
        (
            "ws-active-turn-pending",
            WorkerSessionState::TurnPending,
            "hash-active-turn-pending",
            true,
        ),
        (
            "ws-failed",
            WorkerSessionState::Failed,
            "hash-failed",
            false,
        ),
        (
            "ws-exited",
            WorkerSessionState::Exited,
            "hash-exited",
            false,
        ),
        (
            "ws-superseded",
            WorkerSessionState::Superseded,
            "hash-superseded",
            false,
        ),
    ];
    let mut tx = repo.pool().begin().await.unwrap();
    for (id, state, hash, _) in rows {
        session_insert_tx(&mut tx, worker_session(id, wave.id.clone(), state, hash))
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();

    for (id, _, hash, active) in rows {
        let got = repo.session_get_by_active_token_hash(hash).await.unwrap();
        if active {
            let session = got.expect("active session should resolve by hash");
            assert_eq!(session.id.as_str(), id);
            assert_eq!(session.wave_id, wave.id);
            assert_eq!(session.mcp_token_hash.as_deref(), Some(hash));
        } else {
            assert!(
                got.is_none(),
                "terminal/stale session {id} must not resolve by hash"
            );
        }
    }
}

#[tokio::test]
async fn runtime_start_shared_spec_restarts_wave_root_on_respawn() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let first = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::SharedSpec,
            Some(AgentProvider::Codex),
            WorkerSessionState::Starting,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let root: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM waves WHERE id = ?1")
            .bind(card.wave_id.as_str())
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(root.as_deref(), Some(first.id.as_str()));

    let mut tx = repo.pool().begin().await.unwrap();
    let second = session_supersede_and_start_tx(
        &mut tx,
        &first.id,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::SharedSpec,
            Some(AgentProvider::Codex),
            WorkerSessionState::Starting,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let root: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM waves WHERE id = ?1")
            .bind(card.wave_id.as_str())
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(root.as_deref(), Some(second.id.as_str()));
}

#[tokio::test]
async fn runtime_start_terminal_shared_spec_does_not_stamp_wave_root() {
    let repo = fresh_repo().await;
    let wave = make_wave(&repo).await;
    let failed_card = make_card_in_wave(&repo, wave.id.clone(), "codex").await;

    let mut tx = repo.pool().begin().await.unwrap();
    let failed = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            failed_card.id.to_string(),
            WorkerSessionKind::SharedSpec,
            Some(AgentProvider::Codex),
            WorkerSessionState::Failed,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let root: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM waves WHERE id = ?1")
            .bind(wave.id.as_str())
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(root, None);

    let live_card = make_card_in_wave(&repo, wave.id.clone(), "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let live = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            live_card.id.to_string(),
            WorkerSessionKind::SharedSpec,
            Some(AgentProvider::Codex),
            WorkerSessionState::Starting,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let root: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM waves WHERE id = ?1")
            .bind(wave.id.as_str())
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(root.as_deref(), Some(live.id.as_str()));

    let exited_card = make_card_in_wave(&repo, wave.id.clone(), "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let exited = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            exited_card.id.to_string(),
            WorkerSessionKind::SharedSpec,
            Some(AgentProvider::Codex),
            WorkerSessionState::Exited,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let root: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM waves WHERE id = ?1")
            .bind(wave.id.as_str())
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(root.as_deref(), Some(live.id.as_str()));
    assert_ne!(root.as_deref(), Some(failed.id.as_str()));
    assert_ne!(root.as_deref(), Some(exited.id.as_str()));
}

#[tokio::test]
async fn runtime_start_executor_respawn_leaves_wave_root_unchanged() {
    let repo = fresh_repo().await;
    let wave = make_wave(&repo).await;
    let planner_card = make_card_in_wave(&repo, wave.id.clone(), "codex").await;
    let executor_card = make_card_in_wave(&repo, wave.id.clone(), "codex").await;

    let mut tx = repo.pool().begin().await.unwrap();
    let root = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            planner_card.id.to_string(),
            WorkerSessionKind::SharedSpec,
            Some(AgentProvider::Codex),
            WorkerSessionState::Starting,
        ),
    )
    .await
    .unwrap();
    let executor = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            executor_card.id.to_string(),
            WorkerSessionKind::CodexCard,
            Some(AgentProvider::Codex),
            WorkerSessionState::Starting,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = repo.pool().begin().await.unwrap();
    let replacement = session_supersede_and_start_tx(
        &mut tx,
        &executor.id,
        runtime_init(
            executor_card.id.to_string(),
            WorkerSessionKind::CodexCard,
            Some(AgentProvider::Codex),
            WorkerSessionState::Starting,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let current_root: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM waves WHERE id = ?1")
            .bind(wave.id.as_str())
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(current_root.as_deref(), Some(root.id.as_str()));
    assert_ne!(current_root.as_deref(), Some(replacement.id.as_str()));
}

#[tokio::test]
async fn runtime_start_links_card_to_worker_session() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::CodexCard,
            Some(AgentProvider::Codex),
            WorkerSessionState::Starting,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let linked: Option<String> = sqlx::query_scalar("SELECT session_id FROM cards WHERE id = ?1")
        .bind(card.id.as_str())
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(linked.as_deref(), Some(runtime.id.as_str()));
}

#[tokio::test]
async fn phase1_reorder_cold_start_no_supersede() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let init = runtime_init(
        card.id.to_string(),
        WorkerSessionKind::SharedSpec,
        Some(AgentProvider::Codex),
        WorkerSessionState::Starting,
    );
    let placeholder_id = init.id.clone();

    let mut tx = repo.pool().begin().await.unwrap();
    session_prepare_deferred_spec_tx(&mut tx, &init)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let superseded_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM worker_sessions WHERE card_id = ?1 AND state = 'superseded'",
    )
    .bind(card.id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(superseded_count, 0);

    let active = repo
        .session_projection_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("placeholder should be active");
    assert_eq!(active.id, placeholder_id);
    assert_eq!(active.status, WorkerSessionState::Starting);
}

#[tokio::test]
async fn runtime_restore_repoints_card_and_root_to_restored_session() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let old_hash = "old-restored-token-hash";
    let replacement_hash = "replacement-failed-token-hash";

    let mut tx = repo.pool().begin().await.unwrap();
    let old = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::SharedSpec,
            Some(AgentProvider::Codex),
            WorkerSessionState::Idle,
        ),
    )
    .await
    .unwrap();
    session_mcp_token_set_tx(&mut tx, &old.id, old_hash)
        .await
        .unwrap();
    let replacement = session_supersede_and_start_tx(
        &mut tx,
        &old.id,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::SharedSpec,
            Some(AgentProvider::Codex),
            WorkerSessionState::Starting,
        ),
    )
    .await
    .unwrap();
    session_mcp_token_set_tx(&mut tx, &replacement.id, replacement_hash)
        .await
        .unwrap();
    session_fail_if_active_runtime_tx(&mut tx, &replacement.id)
        .await
        .unwrap();
    session_restore_from_superseded_runtime_tx(&mut tx, &old.id, WorkerSessionState::Idle)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let linked: Option<String> = sqlx::query_scalar("SELECT session_id FROM cards WHERE id = ?1")
        .bind(card.id.as_str())
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(linked.as_deref(), Some(old.id.as_str()));

    let root: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM waves WHERE id = ?1")
            .bind(card.wave_id.as_str())
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(root.as_deref(), Some(old.id.as_str()));

    let restored = repo
        .session_projection_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("restored old runtime should be active");
    assert_eq!(restored.id, old.id);
    assert_eq!(restored.status, WorkerSessionState::Idle);
    assert_eq!(
        repo.session_get_by_active_token_hash(replacement_hash)
            .await
            .unwrap(),
        None,
        "failed replacement token must not resolve as active"
    );

    let session = repo
        .session_get_by_active_token_hash(old_hash)
        .await
        .unwrap()
        .expect("old MCP token should resolve after restore");
    assert_eq!(session.id.as_str(), old.id.as_str());
    let identity = repo
        .card_identity_get_by_session(session.id.as_str())
        .await
        .unwrap()
        .expect("restored session should resolve card identity");
    assert_eq!(identity.card_id, card.id);
    assert_eq!(identity.wave_id, card.wave_id);
}

#[tokio::test]
async fn phase1_reorder_hot_start_supersedes_old_one_row() {
    let repo = fresh_repo().await;
    let active_card = make_card(&repo, "codex").await;

    let mut tx = repo.pool().begin().await.unwrap();
    let old = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            active_card.id.to_string(),
            WorkerSessionKind::SharedSpec,
            Some(AgentProvider::Codex),
            WorkerSessionState::Idle,
        ),
    )
    .await
    .unwrap();
    let placeholder_init = runtime_init(
        active_card.id.to_string(),
        WorkerSessionKind::SharedSpec,
        Some(AgentProvider::Codex),
        WorkerSessionState::Starting,
    );
    let placeholder_id = placeholder_init.id.clone();
    session_prepare_deferred_spec_tx(&mut tx, &placeholder_init)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let active_link: Option<String> =
        sqlx::query_scalar("SELECT session_id FROM cards WHERE id = ?1")
            .bind(active_card.id.as_str())
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(active_link.as_deref(), Some(placeholder_id.as_str()));
    assert_ne!(active_link.as_deref(), Some(old.id.as_str()));
    let old_session = repo
        .session_get(&WorkerSessionId::from(old.id.clone()))
        .await
        .unwrap()
        .expect("old session remains");
    assert_eq!(old_session.state, WorkerSessionState::Superseded);

    let active_root: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM waves WHERE id = ?1")
            .bind(active_card.wave_id.as_str())
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(active_root.as_deref(), Some(placeholder_id.as_str()));
    assert_ne!(active_root.as_deref(), Some(old.id.as_str()));

    let fresh_card = make_card(&repo, "codex").await;
    let fresh_placeholder_init = runtime_init(
        fresh_card.id.to_string(),
        WorkerSessionKind::SharedSpec,
        Some(AgentProvider::Codex),
        WorkerSessionState::Starting,
    );
    let fresh_placeholder_id = fresh_placeholder_init.id.clone();
    let mut tx = repo.pool().begin().await.unwrap();
    session_prepare_deferred_spec_tx(&mut tx, &fresh_placeholder_init)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let fresh_link: Option<String> =
        sqlx::query_scalar("SELECT session_id FROM cards WHERE id = ?1")
            .bind(fresh_card.id.as_str())
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(fresh_link.as_deref(), Some(fresh_placeholder_id.as_str()));
    let fresh_root: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM waves WHERE id = ?1")
            .bind(fresh_card.wave_id.as_str())
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(fresh_root.as_deref(), Some(fresh_placeholder_id.as_str()));
    assert!(
        repo.session_get(&WorkerSessionId::from(fresh_placeholder_id))
            .await
            .unwrap()
            .is_some(),
        "fresh deferred placeholder session should still exist"
    );

    let active_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM worker_sessions WHERE card_id = ?1 AND state IN ('starting','running','idle','turn_pending')",
    )
    .bind(active_card.id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(
        active_count, 1,
        "exactly one active ws per card after Phase-1 reorder"
    );
}

#[tokio::test]
async fn phase2_supersedes_placeholder_one_row() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let placeholder_init = runtime_init(
        card.id.to_string(),
        WorkerSessionKind::SharedSpec,
        Some(AgentProvider::Codex),
        WorkerSessionState::Starting,
    );
    let placeholder_id = placeholder_init.id.clone();
    let mut tx = repo.pool().begin().await.unwrap();
    session_prepare_deferred_spec_tx(&mut tx, &placeholder_init)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut real_init = runtime_init(
        card.id.to_string(),
        WorkerSessionKind::SharedSpec,
        Some(AgentProvider::Codex),
        WorkerSessionState::Idle,
    );
    real_init.id = placeholder_id.clone();
    real_init.thread_id = Some("thread-phase2".into());
    let mut tx = repo.pool().begin().await.unwrap();
    let real = session_supersede_and_start_tx(&mut tx, &placeholder_id, real_init)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let refreshed = repo
        .session_get(&WorkerSessionId::from(placeholder_id.clone()))
        .await
        .unwrap()
        .expect("placeholder session refreshes in place");
    assert_eq!(refreshed.state, WorkerSessionState::Idle);
    assert_eq!(refreshed.thread_id.as_deref(), Some("thread-phase2"));

    let active = repo
        .session_projection_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("real runtime is active");
    assert_eq!(active.id, real.id);
    assert_eq!(active.id, placeholder_id);
    assert_eq!(active.thread_id.as_deref(), Some("thread-phase2"));

    let active_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM worker_sessions
         WHERE card_id = ?1
           AND state IN ('starting', 'running', 'idle', 'turn_pending')",
    )
    .bind(card.id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(active_count, 1);
}

#[tokio::test]
async fn runtime_entrances_dual_write_worker_session() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::CodexCard,
            Some(AgentProvider::Codex),
            WorkerSessionState::Starting,
        ),
    )
    .await
    .unwrap();
    session_bind_attribution_tx(
        &mut tx,
        &runtime.id,
        ThreadAttribution {
            runtime_id: runtime.id.clone(),
            provider: AgentProvider::Codex,
            thread_id: Some("thread-dual-write".into()),
            session_id: Some("agent-session-dual-write".into()),
            active_turn_id: Some("turn-1".into()),
        },
    )
    .await
    .unwrap();
    session_set_status_tx(&mut tx, &runtime.id, WorkerSessionState::Running)
        .await
        .unwrap();
    session_set_active_turn_tx(&mut tx, &runtime.id, Some("turn-2"))
        .await
        .unwrap();
    session_set_handle_state_tx(
        &mut tx,
        &runtime.id,
        Some(json!({"phase": "dual-write", "n": 1})),
    )
    .await
    .unwrap();
    session_complete_tx(&mut tx, &runtime.id, WorkerSessionState::Exited)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let session = repo
        .session_get(&WorkerSessionId(runtime.id.clone()))
        .await
        .unwrap()
        .expect("mirrored worker session");
    assert_eq!(session.state, WorkerSessionState::Exited);
    assert_eq!(session.thread_id.as_deref(), Some("thread-dual-write"));
    assert_eq!(
        session.agent_session_id.as_deref(),
        Some("agent-session-dual-write")
    );
    assert_eq!(session.active_turn_id.as_deref(), Some("turn-2"));
    assert!(session.completed_at_ms.is_some());
}

#[tokio::test]
async fn runtime_tolerant_entrances_dual_write_without_session_matrix() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::SharedSpec,
            Some(AgentProvider::Codex),
            WorkerSessionState::Idle,
        ),
    )
    .await
    .unwrap();
    session_set_harness_observation_runtime_tx(
        &mut tx,
        &runtime.id,
        WorkerSessionState::TurnPending,
        Some("thread-harness"),
        Some("turn-harness"),
    )
    .await
    .unwrap();
    session_fail_if_active_runtime_tx(&mut tx, &runtime.id)
        .await
        .unwrap();
    session_mark_superseded_runtime_tx(&mut tx, &runtime.id)
        .await
        .unwrap();
    session_restore_from_superseded_runtime_tx(&mut tx, &runtime.id, WorkerSessionState::Running)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let session = repo
        .session_get(&WorkerSessionId(runtime.id))
        .await
        .unwrap()
        .expect("mirrored worker session");
    assert_eq!(session.state, WorkerSessionState::Running);
    assert_eq!(session.thread_id.as_deref(), Some("thread-harness"));
    assert_eq!(session.active_turn_id.as_deref(), Some("turn-harness"));
    assert!(session.completed_at_ms.is_none());
}

#[tokio::test]
async fn session_supersede_and_start_tx_mirrors_old_superseded_and_new_starting_same_wave() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let first = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::CodexCard,
            Some(AgentProvider::Codex),
            WorkerSessionState::Starting,
        ),
    )
    .await
    .unwrap();
    let second = session_supersede_and_start_tx(
        &mut tx,
        &first.id,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::CodexCard,
            Some(AgentProvider::Codex),
            WorkerSessionState::Starting,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        r#"SELECT id, state, wave_id
           FROM worker_sessions
           WHERE id IN (?1, ?2)
           ORDER BY id ASC"#,
    )
    .bind(&first.id)
    .bind(&second.id)
    .fetch_all(repo.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    let first_row = rows.iter().find(|(id, _, _)| id == &first.id).unwrap();
    let second_row = rows.iter().find(|(id, _, _)| id == &second.id).unwrap();
    assert_eq!(first_row.1, "superseded");
    assert_eq!(second_row.1, "starting");
    assert_eq!(first_row.2, second_row.2);
}

#[tokio::test]
async fn stale_harness_observation_cannot_revive_superseded_runtime() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "spec").await;
    let card_id = card.id.to_string();
    let mut tx = repo.pool().begin().await.unwrap();
    let old = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            card_id.clone(),
            WorkerSessionKind::SharedSpec,
            Some(AgentProvider::Codex),
            WorkerSessionState::Idle,
        ),
    )
    .await
    .unwrap();
    session_mark_superseded_runtime_tx(&mut tx, &old.id)
        .await
        .unwrap();

    session_set_harness_observation_runtime_tx(
        &mut tx,
        &old.id,
        WorkerSessionState::Running,
        Some("stale-thread"),
        Some("stale-turn"),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let old_after = runtime_by_id_tx_snapshot(&repo, &old.id)
        .await
        .expect("old runtime");
    assert_eq!(old_after.status, WorkerSessionState::Superseded);
    assert_eq!(old_after.thread_id, None);
    assert_eq!(old_after.active_turn_id, None);

    let old_session = repo
        .session_get(&WorkerSessionId(old.id.clone()))
        .await
        .unwrap()
        .expect("mirrored old worker session");
    assert_eq!(old_session.state, WorkerSessionState::Superseded);
    assert_eq!(old_session.thread_id, None);
    assert_eq!(old_session.active_turn_id, None);

    let active = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap();
    assert!(active.is_none());

    let active_count: (i64,) = sqlx::query_as(
        r#"SELECT COUNT(*)
           FROM worker_sessions
          WHERE card_id = ?1
            AND state IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(card_id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(active_count.0, 0);

    let replacement_card = make_card(&repo, "spec").await;
    let replacement_card_id = replacement_card.id.to_string();
    let mut tx = repo.pool().begin().await.unwrap();
    let replaced_old = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            replacement_card_id.clone(),
            WorkerSessionKind::SharedSpec,
            Some(AgentProvider::Codex),
            WorkerSessionState::Idle,
        ),
    )
    .await
    .unwrap();
    let replacement = session_supersede_and_start_tx(
        &mut tx,
        &replaced_old.id,
        runtime_init(
            replacement_card_id.clone(),
            WorkerSessionKind::SharedSpec,
            Some(AgentProvider::Codex),
            WorkerSessionState::Starting,
        ),
    )
    .await
    .unwrap();

    session_set_harness_observation_runtime_tx(
        &mut tx,
        &replaced_old.id,
        WorkerSessionState::Running,
        Some("stale-replaced-thread"),
        Some("stale-replaced-turn"),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let replaced_old_after = runtime_by_id_tx_snapshot(&repo, &replaced_old.id)
        .await
        .expect("replaced old runtime");
    assert_eq!(replaced_old_after.status, WorkerSessionState::Superseded);
    assert_eq!(replaced_old_after.thread_id, None);
    assert_eq!(replaced_old_after.active_turn_id, None);

    let replaced_old_session = repo
        .session_get(&WorkerSessionId(replaced_old.id.clone()))
        .await
        .unwrap()
        .expect("mirrored replaced old worker session");
    assert_eq!(replaced_old_session.state, WorkerSessionState::Superseded);
    assert_eq!(replaced_old_session.thread_id, None);
    assert_eq!(replaced_old_session.active_turn_id, None);

    let replacement_active = repo
        .session_projection_active_for_card(&replacement_card_id)
        .await
        .unwrap()
        .expect("replacement active runtime");
    assert_eq!(replacement_active.id, replacement.id);

    let replacement_active_count: (i64,) = sqlx::query_as(
        r#"SELECT COUNT(*)
           FROM worker_sessions
          WHERE card_id = ?1
            AND state IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(replacement_card_id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(replacement_active_count.0, 1);
}

#[tokio::test]
async fn session_start_runtime_tx_terminal_persists_active_row() {
    let repo = fresh_repo().await;
    let wave = make_wave(&repo).await;
    let mut tx = repo.pool().begin().await.unwrap();
    let (card, term) = card_with_terminal_create_tx(
        &mut tx,
        new_id(),
        &new_id(),
        None,
        wave.id,
        None,
        "bash".into(),
        "/tmp".into(),
        json!({}),
        CardRole::Worker,
        true,
        repo.card_role_cache(),
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM worker_sessions WHERE card_id = ?1")
        .bind(card.id.as_str())
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(count.0, 1);

    let active = repo
        .session_projection_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("active runtime");
    assert_eq!(active.kind, WorkerSessionKind::Terminal);
    assert_eq!(active.status, WorkerSessionState::Starting);
    assert_eq!(active.terminal_run_id.as_deref(), Some(term.id.as_str()));
}

#[tokio::test]
async fn runtime_complete_for_terminal_exited_path() {
    let repo = fresh_repo().await;
    let wave = make_wave(&repo).await;
    let mut tx = repo.pool().begin().await.unwrap();
    let (card, term) = card_with_terminal_create_tx(
        &mut tx,
        new_id(),
        &new_id(),
        None,
        wave.id,
        None,
        "bash".into(),
        "/tmp".into(),
        json!({}),
        CardRole::Worker,
        true,
        repo.card_role_cache(),
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let active = repo
        .session_projection_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("active runtime");
    repo.session_projection_complete_for_terminal(&term.id, WorkerSessionState::Exited)
        .await
        .unwrap();

    let completed = repo
        .session_projection_by_id(&active.id)
        .await
        .unwrap()
        .expect("runtime");
    assert_eq!(completed.kind, WorkerSessionKind::Terminal);
    assert_eq!(completed.status, WorkerSessionState::Exited);
    assert_eq!(completed.terminal_run_id.as_deref(), Some(term.id.as_str()));
    assert!(completed.completed_at_ms.is_some());
    assert!(
        repo.session_projection_active_for_card(&card.id.to_string())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn runtime_complete_for_terminal_failed_path() {
    let repo = fresh_repo().await;
    let wave = make_wave(&repo).await;
    let mut tx = repo.pool().begin().await.unwrap();
    let (card, term) = card_with_terminal_create_tx(
        &mut tx,
        new_id(),
        &new_id(),
        None,
        wave.id,
        None,
        "bash".into(),
        "/tmp".into(),
        json!({}),
        CardRole::Worker,
        true,
        repo.card_role_cache(),
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let active = repo
        .session_projection_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("active runtime");
    repo.session_projection_complete_for_terminal(&term.id, WorkerSessionState::Failed)
        .await
        .unwrap();

    let completed = repo
        .session_projection_by_id(&active.id)
        .await
        .unwrap()
        .expect("runtime");
    assert_eq!(completed.status, WorkerSessionState::Failed);
    assert_eq!(completed.terminal_run_id.as_deref(), Some(term.id.as_str()));
    assert!(completed.completed_at_ms.is_some());
}

#[tokio::test]
async fn runtime_complete_for_terminal_noop_when_no_active() {
    let repo = fresh_repo().await;
    repo.session_projection_complete_for_terminal("missing-terminal", WorkerSessionState::Exited)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn runtime_set_status_for_card_noop_when_no_active() {
    let repo = fresh_repo().await;
    let wave = make_wave(&repo).await;
    let mut tx = repo.pool().begin().await.unwrap();
    let (card, _term) = card_with_terminal_create_tx(
        &mut tx,
        new_id(),
        &new_id(),
        None,
        wave.id,
        None,
        "bash".into(),
        "/tmp".into(),
        json!({}),
        CardRole::Worker,
        true,
        repo.card_role_cache(),
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .unwrap();
    let runtime_id = session_projection_active_for_card_tx(&mut tx, card.id.as_ref())
        .await
        .unwrap()
        .expect("active runtime")
        .id;
    session_complete_tx(&mut tx, &runtime_id, WorkerSessionState::Exited)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let before = runtime_row_snapshot(&repo, &runtime_id).await;
    let mut tx = repo.pool().begin().await.unwrap();
    session_set_status_for_card_tx(&mut tx, card.id.as_ref(), WorkerSessionState::Running)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let after = runtime_row_snapshot(&repo, &runtime_id).await;

    assert_eq!(before, after);
    assert_eq!(after.0, "exited");
    assert!(
        repo.session_projection_active_for_card(&card.id.to_string())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn runtime_complete_for_card_noop_when_no_active() {
    let repo = fresh_repo().await;
    let wave = make_wave(&repo).await;
    let mut tx = repo.pool().begin().await.unwrap();
    let (card, _term) = card_with_terminal_create_tx(
        &mut tx,
        new_id(),
        &new_id(),
        None,
        wave.id,
        None,
        "bash".into(),
        "/tmp".into(),
        json!({}),
        CardRole::Worker,
        true,
        repo.card_role_cache(),
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .unwrap();
    let runtime_id = session_projection_active_for_card_tx(&mut tx, card.id.as_ref())
        .await
        .unwrap()
        .expect("active runtime")
        .id;
    session_complete_tx(&mut tx, &runtime_id, WorkerSessionState::Exited)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let before = runtime_row_snapshot(&repo, &runtime_id).await;
    let mut tx = repo.pool().begin().await.unwrap();
    session_complete_for_card_tx(&mut tx, card.id.as_ref(), WorkerSessionState::Failed)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let after = runtime_row_snapshot(&repo, &runtime_id).await;

    assert_eq!(before, after);
    assert_eq!(after.0, "exited");
    assert!(
        repo.session_projection_active_for_card(&card.id.to_string())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn runtime_card_lifecycle_helpers_mark_running_and_failed() {
    let repo = fresh_repo().await;
    let wave = make_wave(&repo).await;
    let mut tx = repo.pool().begin().await.unwrap();
    let (card, _term) = card_with_terminal_create_tx(
        &mut tx,
        new_id(),
        &new_id(),
        None,
        wave.id,
        None,
        "bash".into(),
        "/tmp".into(),
        json!({}),
        CardRole::Worker,
        true,
        repo.card_role_cache(),
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .unwrap();
    let runtime_id = session_projection_active_for_card_tx(&mut tx, card.id.as_ref())
        .await
        .unwrap()
        .expect("active runtime")
        .id;
    session_set_status_for_card_tx(&mut tx, card.id.as_ref(), WorkerSessionState::Running)
        .await
        .unwrap();
    session_complete_for_card_tx(&mut tx, card.id.as_ref(), WorkerSessionState::Failed)
        .await
        .unwrap();
    let completed = session_projection_by_id_tx(&mut tx, &runtime_id)
        .await
        .unwrap()
        .expect("completed runtime");
    let active_after_complete = session_projection_active_for_card_tx(&mut tx, card.id.as_ref())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(completed.status, WorkerSessionState::Failed);
    assert!(completed.completed_at_ms.is_some());
    assert!(active_after_complete.is_none());
    let row: (String, Option<i64>) =
        sqlx::query_as("SELECT state, completed_at_ms FROM worker_sessions WHERE card_id = ?1")
            .bind(card.id.as_str())
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(row.0, "failed");
    assert!(row.1.is_some());
}

#[tokio::test]
async fn runtime_codex_helper_writes_starting_with_terminal_ref() {
    let repo = fresh_repo().await;
    let wave = make_wave(&repo).await;
    let mut tx = repo.pool().begin().await.unwrap();
    let (card, term, _token) = card_with_codex_create_tx(
        &mut tx,
        new_id(),
        &new_id(),
        None,
        wave.id,
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
    .unwrap();
    tx.commit().await.unwrap();

    let active = repo
        .session_projection_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("active runtime");
    assert_eq!(active.kind, WorkerSessionKind::CodexCard);
    assert_eq!(active.status, WorkerSessionState::Starting);
    assert_eq!(active.terminal_run_id.as_deref(), Some(term.id.as_str()));
    assert!(active.thread_id.is_none());
}

#[tokio::test]
async fn worker_session_spawn_op_id_stamped_only_for_worker_mints() {
    let repo = fresh_repo().await;
    let codex_op_id = new_id();
    ensure_test_operation(&repo, &codex_op_id, "codex-worker", "task-codex").await;
    let (codex_id, _) = mint_codex_session(&repo, Some(&codex_op_id)).await;
    let codex_session = repo
        .session_get(&codex_id)
        .await
        .unwrap()
        .expect("codex worker session");
    // `spawn_op_id` stores operations.id; operations.idempotency_key resolves to task.id separately.
    assert_eq!(
        codex_session.spawn_op_id.as_deref(),
        Some(codex_op_id.as_str())
    );

    let terminal_op_id = new_id();
    ensure_test_operation(&repo, &terminal_op_id, "terminal-worker", "task-terminal").await;
    let (terminal_id, _) = mint_terminal_session(&repo, Some(&terminal_op_id)).await;
    let terminal_session = repo
        .session_get(&terminal_id)
        .await
        .unwrap()
        .expect("terminal worker session");
    assert_eq!(
        terminal_session.spawn_op_id.as_deref(),
        Some(terminal_op_id.as_str())
    );

    let (codex_create_id, _) = mint_codex_session(&repo, None).await;
    let codex_create_session = repo
        .session_get(&codex_create_id)
        .await
        .unwrap()
        .expect("codex-create session");
    assert_eq!(codex_create_session.spawn_op_id, None);

    let spec_card = make_card(&repo, "codex").await;
    let spec_runtime_id = new_id();
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: spec_runtime_id.clone(),
            card_id: spec_card.id.to_string(),
            kind: WorkerSessionKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Starting,
            terminal_run_id: None,
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let spec_session = repo
        .session_get(&WorkerSessionId(spec_runtime_id))
        .await
        .unwrap()
        .expect("spec harness session");
    assert_eq!(spec_session.spawn_op_id, None);
}

#[tokio::test]
async fn session_commit_exit_commits_session_and_runtime_in_lockstep() {
    for (interpretation, exit_code) in [
        (ExitInterpretation::Completed, Some(0)),
        (
            ExitInterpretation::Failed {
                reason: "provider exited".into(),
            },
            Some(2),
        ),
    ] {
        let repo = fresh_repo().await;
        let (session_id, _) = mint_terminal_session(&repo, Some("task-exit")).await;
        let probe_ms = now_ms() + 10;
        let expected_state = match &interpretation {
            ExitInterpretation::Completed => WorkerSessionState::Exited,
            ExitInterpretation::Failed { .. } => WorkerSessionState::Failed,
            ExitInterpretation::PreserveCard | ExitInterpretation::ResumeEligible => {
                unreachable!("test only covers committed exit interpretations")
            }
        };
        let expected_exit_interpretation = interpretation.as_db_str();
        let outcome = repo
            .session_commit_exit(
                &session_id,
                expected_state,
                probe_ms,
                exit_code,
                expected_exit_interpretation,
            )
            .await
            .unwrap();
        let committed = match outcome {
            CommitExitOutcome::Committed(session) => session,
            CommitExitOutcome::Absorbed => panic!("exit commit should win"),
        };
        assert_eq!(committed.state, expected_state);
        assert_eq!(committed.liveness, LivenessTag::Exited);
        assert_eq!(committed.liveness_probed_at_ms, Some(probe_ms));
        assert_eq!(committed.exit_code, exit_code);
        assert_eq!(
            committed.exit_interpretation.as_deref(),
            Some(expected_exit_interpretation)
        );
        assert_eq!(committed.updated_at_ms, probe_ms);
        assert_eq!(committed.completed_at_ms, Some(probe_ms));

        let runtime = repo
            .session_projection_by_id(&session_id.0)
            .await
            .unwrap()
            .expect("runtime row");
        assert_eq!(runtime.status, expected_state);
        assert_eq!(runtime.updated_at_ms, probe_ms);
        assert_eq!(runtime.completed_at_ms, Some(probe_ms));
    }
}

#[tokio::test]
async fn session_commit_exit_absorbs_lost_session_race_without_clobber() {
    let repo = fresh_repo().await;
    let (session_id, _) = mint_terminal_session(&repo, Some("task-race")).await;
    let mut tx = repo.pool().begin().await.unwrap();
    session_complete_tx(&mut tx, &session_id.0, WorkerSessionState::Exited)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let before = repo
        .session_get(&session_id)
        .await
        .unwrap()
        .expect("terminal session before absorbed commit");

    let outcome = repo
        .session_commit_exit(
            &session_id,
            WorkerSessionState::Failed,
            now_ms() + 30,
            Some(9),
            "failed",
        )
        .await
        .unwrap();
    assert_eq!(outcome, CommitExitOutcome::Absorbed);
    let after = repo
        .session_get(&session_id)
        .await
        .unwrap()
        .expect("terminal session after absorbed commit");
    assert_eq!(after, before);
}

#[tokio::test]
async fn session_commit_exit_tx_conflicts_from_terminal_state() {
    let repo = fresh_repo().await;
    let (session_id, _) = mint_terminal_session(&repo, Some("task-illegal")).await;
    let mut tx = repo.pool().begin().await.unwrap();
    session_complete_tx(&mut tx, &session_id.0, WorkerSessionState::Exited)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = repo.pool().begin().await.unwrap();
    let err = session_commit_exit_tx(
        &mut tx,
        &session_id,
        WorkerSessionState::Failed,
        now_ms() + 40,
        Some(1),
        "failed",
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err,
        calm_truth::error::CalmError::Core(calm_types::error::CoreError::Conflict(_))
    ));
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn concurrent_session_commit_exit_has_single_winner() {
    let repo = fresh_repo().await;
    let (session_id, _) = mint_terminal_session(&repo, Some("task-concurrent")).await;
    let left_id = session_id.clone();
    let right_id = session_id.clone();
    let probe_ms = now_ms() + 50;
    let left = repo.session_commit_exit(
        &left_id,
        WorkerSessionState::Exited,
        probe_ms,
        Some(0),
        "completed",
    );
    let right = repo.session_commit_exit(
        &right_id,
        WorkerSessionState::Exited,
        probe_ms,
        Some(0),
        "completed",
    );
    let (left, right) = tokio::join!(left, right);
    let outcomes = [left.unwrap(), right.unwrap()];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, CommitExitOutcome::Committed(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, CommitExitOutcome::Absorbed))
            .count(),
        1
    );
    let session = repo
        .session_get(&session_id)
        .await
        .unwrap()
        .expect("committed session");
    assert_eq!(session.state, WorkerSessionState::Exited);
    assert_eq!(session.updated_at_ms, probe_ms);
}

#[tokio::test]
async fn ws_unique_active_per_card_blocks_concurrent_double_spawn() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();

    session_start_runtime_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::CodexCard,
            Some(AgentProvider::Codex),
            WorkerSessionState::Running,
        ),
    )
    .await
    .unwrap();
    let err = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::CodexCard,
            Some(AgentProvider::Codex),
            WorkerSessionState::Running,
        ),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err,
        WorkerSessionProjectionRepoError::Message { message }
            if message.contains("worker_sessions.card_id") || message.contains("UNIQUE")
    ));
}

#[tokio::test]
async fn session_supersede_and_start_tx_atomic() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let first = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::CodexCard,
            Some(AgentProvider::Codex),
            WorkerSessionState::Starting,
        ),
    )
    .await
    .unwrap();
    let second_init = runtime_init(
        card.id.to_string(),
        WorkerSessionKind::CodexCard,
        Some(AgentProvider::Codex),
        WorkerSessionState::Running,
    );
    let second = session_supersede_and_start_tx(&mut tx, &first.id, second_init)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let old = runtime_by_id_tx_snapshot(&repo, &first.id)
        .await
        .expect("old runtime");
    assert_eq!(old.status, WorkerSessionState::Superseded);
    assert_eq!(second.status, WorkerSessionState::Running);

    let active_count: (i64,) = sqlx::query_as(
        r#"SELECT COUNT(*) FROM worker_sessions
           WHERE card_id = ?1
             AND state IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(card.id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(active_count.0, 1);
}

#[tokio::test]
async fn runtime_set_status_superseded_rejected() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::CodexCard,
            Some(AgentProvider::Codex),
            WorkerSessionState::Running,
        ),
    )
    .await
    .unwrap();

    let err = session_set_status_tx(&mut tx, &runtime.id, WorkerSessionState::Superseded)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        WorkerSessionProjectionRepoError::IllegalStatusTransition {
            attempted: WorkerSessionState::Superseded,
            ..
        }
    ));
}

#[tokio::test]
async fn runtime_set_status_same_running_rejected() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::CodexCard,
            Some(AgentProvider::Codex),
            WorkerSessionState::Running,
        ),
    )
    .await
    .unwrap();

    let err = session_set_status_tx(&mut tx, &runtime.id, WorkerSessionState::Running)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        WorkerSessionProjectionRepoError::IllegalStatusTransition {
            attempted: WorkerSessionState::Running,
            ..
        }
    ));
}

#[tokio::test]
async fn runtime_bind_attribution_transitions_pending_to_running() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::CodexCard,
            Some(AgentProvider::Codex),
            WorkerSessionState::TurnPending,
        ),
    )
    .await
    .unwrap();
    session_bind_attribution_tx(
        &mut tx,
        &runtime.id,
        ThreadAttribution {
            runtime_id: runtime.id.clone(),
            provider: AgentProvider::Codex,
            thread_id: Some("thread-pending-bind".into()),
            session_id: None,
            active_turn_id: None,
        },
    )
    .await
    .unwrap();
    session_set_status_tx(&mut tx, &runtime.id, WorkerSessionState::Running)
        .await
        .unwrap();
    let persisted = session_projection_by_id_tx(&mut tx, &runtime.id)
        .await
        .unwrap()
        .expect("runtime");
    tx.commit().await.unwrap();

    assert_eq!(persisted.status, WorkerSessionState::Running);
    assert_eq!(persisted.thread_id.as_deref(), Some("thread-pending-bind"));
}

#[tokio::test]
async fn session_start_runtime_tx_codex_empty_is_turn_pending() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::CodexCard,
            Some(AgentProvider::Codex),
            WorkerSessionState::TurnPending,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let persisted = repo
        .session_projection_by_id(&runtime.id)
        .await
        .unwrap()
        .expect("runtime");
    assert_eq!(persisted.status, WorkerSessionState::TurnPending);
    assert!(persisted.thread_id.is_none());
}

#[tokio::test]
async fn runtime_pending_drop_completes_failed() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::CodexCard,
            Some(AgentProvider::Codex),
            WorkerSessionState::TurnPending,
        ),
    )
    .await
    .unwrap();
    session_complete_tx(&mut tx, &runtime.id, WorkerSessionState::Failed)
        .await
        .unwrap();
    let completed = session_projection_by_id_tx(&mut tx, &runtime.id)
        .await
        .unwrap()
        .expect("runtime");
    tx.commit().await.unwrap();

    assert_eq!(completed.status, WorkerSessionState::Failed);
    assert!(completed.completed_at_ms.is_some());
}

#[tokio::test]
async fn session_start_runtime_tx_claude_records_session_when_present() {
    let repo = fresh_repo().await;
    let wave = make_wave(&repo).await;
    let session_id = "11111111-1111-4111-8111-111111111111".to_string();
    let mut tx = repo.pool().begin().await.unwrap();
    let (card, term) = card_with_claude_create_tx(
        &mut tx,
        new_id(),
        &new_id(),
        wave.id,
        None,
        "claude --session-id".into(),
        "/workspace".into(),
        json!({"NEIGE_HOOK_PROVIDER": "claude"}),
        None,
        None,
        None,
        "/tmp/claude-settings.json".into(),
        session_id.clone(),
        CardRole::Worker,
        true,
        repo.card_role_cache(),
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let active = repo
        .session_projection_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("active runtime");
    assert_eq!(active.kind, WorkerSessionKind::ClaudeCard);
    assert_eq!(active.status, WorkerSessionState::Starting);
    assert_eq!(active.terminal_run_id.as_deref(), Some(term.id.as_str()));
    assert_eq!(active.session_id.as_deref(), Some(session_id.as_str()));

    let mut stored = repo
        .card_get(card.id.as_str())
        .await
        .unwrap()
        .expect("card row");
    assert!(
        stored.payload.get("terminal_id").is_none(),
        "terminal_id must not be persisted in cards.payload: {}",
        stored.payload
    );
    assert!(
        stored.payload.get("claude_session_id").is_none(),
        "claude_session_id must not be persisted in cards.payload: {}",
        stored.payload
    );
    project_runtime_into_card_payload(&repo, &mut stored)
        .await
        .unwrap();
    let runtime = stored.runtime.as_ref().expect("projected card runtime");
    assert_eq!(runtime.runtime_id, active.id);
    assert_eq!(runtime.kind, WorkerSessionKind::ClaudeCard);
    assert_eq!(runtime.status, WorkerSessionState::Starting);
    assert_eq!(runtime.provider, Some(AgentProvider::Claude));
    assert_eq!(runtime.terminal_id.as_deref(), Some(term.id.as_str()));
    assert_eq!(runtime.session_id.as_deref(), Some(session_id.as_str()));
    assert!(runtime.thread_id.is_none());
    assert!(runtime.source.is_none());
    assert!(runtime.thread_status.is_none());
    assert_eq!(stored.payload["terminal_id"], term.id);
    assert_eq!(stored.payload["claude_session_id"], session_id);
    let session = repo
        .session_get(&WorkerSessionId(active.id))
        .await
        .unwrap()
        .expect("mirrored worker session");
    assert_eq!(
        session.agent_session_id.as_deref(),
        Some(session_id.as_str())
    );
}

#[tokio::test]
async fn runtime_handle_state_json_roundtrip() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let state = json!({"phase": "claimed", "queue": [1, 2, 3]});
    let mut init = runtime_init(
        card.id.to_string(),
        WorkerSessionKind::CodexCard,
        Some(AgentProvider::Codex),
        WorkerSessionState::Running,
    );
    init.handle_state_json = Some(state.clone());

    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_start_runtime_tx(&mut tx, init).await.unwrap();
    tx.commit().await.unwrap();

    let persisted = repo
        .session_projection_by_id(&runtime.id)
        .await
        .unwrap()
        .expect("runtime");
    assert_eq!(persisted.handle_state_json, Some(state));
}

#[tokio::test]
async fn session_set_handle_state_tx_writes_active_runtime() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let state = json!({"phase": "active-write", "queue": [1, 2, 3]});

    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::CodexCard,
            Some(AgentProvider::Codex),
            WorkerSessionState::Running,
        ),
    )
    .await
    .unwrap();
    session_set_handle_state_tx(&mut tx, &runtime.id, Some(state.clone()))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let persisted = repo
        .session_projection_by_id(&runtime.id)
        .await
        .unwrap()
        .expect("runtime");
    assert_eq!(persisted.handle_state_json, Some(state.clone()));

    let session = repo
        .session_get(&WorkerSessionId(runtime.id))
        .await
        .unwrap()
        .expect("mirrored worker session");
    assert_eq!(session.handle_state_json, Some(state));
}

#[tokio::test]
async fn session_set_handle_state_tx_noops_for_superseded_runtime() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let original = json!({"phase": "original", "queue": [1]});
    let stale = json!({"phase": "stale", "queue": [2]});
    let mut init = runtime_init(
        card.id.to_string(),
        WorkerSessionKind::CodexCard,
        Some(AgentProvider::Codex),
        WorkerSessionState::Running,
    );
    init.handle_state_json = Some(original.clone());

    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_start_runtime_tx(&mut tx, init).await.unwrap();
    let _replacement = session_supersede_and_start_tx(
        &mut tx,
        &runtime.id,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::CodexCard,
            Some(AgentProvider::Codex),
            WorkerSessionState::Starting,
        ),
    )
    .await
    .unwrap();
    session_set_handle_state_tx(&mut tx, &runtime.id, Some(stale))
        .await
        .expect("superseded handle-state write should no-op");
    tx.commit().await.unwrap();

    let stale_runtime = runtime_by_id_tx_snapshot(&repo, &runtime.id)
        .await
        .expect("superseded runtime");
    assert_eq!(stale_runtime.status, WorkerSessionState::Superseded);
    assert_eq!(stale_runtime.handle_state_json, Some(original.clone()));

    let stale_session = repo
        .session_get(&WorkerSessionId(runtime.id))
        .await
        .unwrap()
        .expect("mirrored superseded worker session");
    assert_eq!(stale_session.state, WorkerSessionState::Superseded);
    assert_eq!(stale_session.handle_state_json, Some(original));
}

#[tokio::test]
async fn session_start_runtime_tx_shared_spec_thread_present_running() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut init = runtime_init(
        card.id.to_string(),
        WorkerSessionKind::SharedSpec,
        Some(AgentProvider::Codex),
        WorkerSessionState::Running,
    );
    init.thread_id = Some("thread-1".into());

    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_start_runtime_tx(&mut tx, init).await.unwrap();
    tx.commit().await.unwrap();

    assert_eq!(runtime.kind, WorkerSessionKind::SharedSpec);
    assert_eq!(runtime.status, WorkerSessionState::Running);
    assert_eq!(runtime.thread_id.as_deref(), Some("thread-1"));
}

#[tokio::test]
async fn projection_overwrites_stale_legacy_keys_from_runtime() {
    let repo = fresh_repo().await;
    let wave = make_wave(&repo).await;
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id,
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "schemaVersion": 1,
                "terminal_id": "OLD",
                "codex_thread_status": "pending_thread_start",
            }),
        })
        .await
        .expect("create card");
    sqlx::query(
        r#"INSERT INTO terminals
               (id, card_id, program, cwd, env, pid, theme_fg, theme_bg, created_at)
           VALUES ('NEW', ?1, 'codex', '/tmp', '{}', NULL, '255,255,255', '0,0,0', ?2)"#,
    )
    .bind(card.id.as_str())
    .bind(now_ms())
    .execute(repo.pool())
    .await
    .expect("insert terminal");

    let mut init = runtime_init(
        card.id.to_string(),
        WorkerSessionKind::CodexCard,
        Some(AgentProvider::Codex),
        WorkerSessionState::Running,
    );
    init.terminal_run_id = Some("NEW".into());
    init.thread_id = Some("abc".into());

    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(&mut tx, init).await.unwrap();
    tx.commit().await.unwrap();

    let mut projected = repo
        .card_get(card.id.as_str())
        .await
        .unwrap()
        .expect("card row");
    project_runtime_into_card_payload(&repo, &mut projected)
        .await
        .unwrap();
    let runtime = projected.runtime.as_ref().expect("projected card runtime");
    assert_eq!(runtime.kind, WorkerSessionKind::CodexCard);
    assert_eq!(runtime.status, WorkerSessionState::Running);
    assert_eq!(runtime.provider, Some(AgentProvider::Codex));
    assert_eq!(runtime.terminal_id.as_deref(), Some("NEW"));
    assert_eq!(runtime.thread_id.as_deref(), Some("abc"));
    assert!(runtime.source.is_none());
    assert_eq!(runtime.thread_status.as_deref(), Some("started"));
    assert_eq!(projected.payload["terminal_id"], "NEW");
    assert_eq!(projected.payload["codex_thread_id"], "abc");
    assert_eq!(projected.payload["codex_thread_status"], "started");

    let once = projected.payload.clone();
    project_runtime_into_card_payload(&repo, &mut projected)
        .await
        .unwrap();
    assert_eq!(projected.payload, once);
}

#[tokio::test]
async fn projection_prefers_active_runtime_over_failed_no_thread() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let failed = runtime_init(
        card.id.to_string(),
        WorkerSessionKind::SharedSpec,
        Some(AgentProvider::Codex),
        WorkerSessionState::Failed,
    );
    let mut active = runtime_init(
        card.id.to_string(),
        WorkerSessionKind::SharedSpec,
        Some(AgentProvider::Codex),
        WorkerSessionState::Running,
    );
    active.thread_id = Some("active-thread".into());

    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(&mut tx, failed).await.unwrap();
    session_start_runtime_tx(&mut tx, active).await.unwrap();
    tx.commit().await.unwrap();

    let mut projected = repo
        .card_get(card.id.as_str())
        .await
        .unwrap()
        .expect("card row");
    project_runtime_into_card_payload(&repo, &mut projected)
        .await
        .unwrap();
    let runtime = projected.runtime.as_ref().expect("projected card runtime");
    assert_eq!(runtime.kind, WorkerSessionKind::SharedSpec);
    assert_eq!(runtime.status, WorkerSessionState::Running);
    assert_eq!(runtime.provider, Some(AgentProvider::Codex));
    assert!(runtime.terminal_id.is_none());
    assert_eq!(runtime.thread_id.as_deref(), Some("active-thread"));
    assert_eq!(runtime.source.as_deref(), Some("shared"));
    assert_eq!(runtime.thread_status.as_deref(), Some("started"));
    assert_eq!(projected.payload["codex_thread_id"], "active-thread");
    assert_eq!(projected.payload["codex_source"], "shared");
    assert_eq!(projected.payload["codex_thread_status"], "started");
}

#[tokio::test]
async fn runtime_shared_spec_reset_supersedes_active_runtime() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut first_init = runtime_init(
        card.id.to_string(),
        WorkerSessionKind::SharedSpec,
        Some(AgentProvider::Codex),
        WorkerSessionState::Running,
    );
    first_init.thread_id = Some("T1".into());
    let mut second_init = runtime_init(
        card.id.to_string(),
        WorkerSessionKind::SharedSpec,
        Some(AgentProvider::Codex),
        WorkerSessionState::Running,
    );
    second_init.thread_id = Some("T2".into());

    let mut tx = repo.pool().begin().await.unwrap();
    let first = session_start_runtime_tx(&mut tx, first_init).await.unwrap();
    let second = session_supersede_and_start_tx(&mut tx, &first.id, second_init)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let old = runtime_by_id_tx_snapshot(&repo, &first.id)
        .await
        .expect("old runtime");
    assert_eq!(old.status, WorkerSessionState::Superseded);
    assert_eq!(old.thread_id.as_deref(), Some("T1"));

    let active = repo
        .session_projection_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("active runtime");
    assert_eq!(active.id, second.id);
    assert_eq!(active.kind, WorkerSessionKind::SharedSpec);
    assert_eq!(active.status, WorkerSessionState::Running);
    assert_eq!(active.thread_id.as_deref(), Some("T2"));

    let active_count: (i64,) = sqlx::query_as(
        r#"SELECT COUNT(*) FROM worker_sessions
           WHERE card_id = ?1
             AND state IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(card.id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(active_count.0, 1);
}

#[tokio::test]
async fn session_start_runtime_tx_shared_spec_absent_turn_pending() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::SharedSpec,
            Some(AgentProvider::Codex),
            WorkerSessionState::TurnPending,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(runtime.status, WorkerSessionState::TurnPending);
    assert!(runtime.thread_id.is_none());
}

#[tokio::test]
async fn session_complete_tx_marks_completed_at() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::CodexCard,
            Some(AgentProvider::Codex),
            WorkerSessionState::Running,
        ),
    )
    .await
    .unwrap();
    session_complete_tx(&mut tx, &runtime.id, WorkerSessionState::Exited)
        .await
        .unwrap();
    let completed = session_projection_by_id_tx(&mut tx, &runtime.id)
        .await
        .unwrap()
        .expect("runtime");
    tx.commit().await.unwrap();

    assert_eq!(completed.status, WorkerSessionState::Exited);
    assert!(completed.completed_at_ms.is_some());
    assert!(completed.completed_at_ms.unwrap() >= completed.created_at_ms);
}

#[tokio::test]
async fn runtime_get_active_for_card_returns_none_when_only_superseded() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let first = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::CodexCard,
            Some(AgentProvider::Codex),
            WorkerSessionState::Running,
        ),
    )
    .await
    .unwrap();
    let second = session_supersede_and_start_tx(
        &mut tx,
        &first.id,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::CodexCard,
            Some(AgentProvider::Codex),
            WorkerSessionState::Running,
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        session_projection_active_for_card_tx(&mut tx, card.id.as_str())
            .await
            .unwrap()
            .expect("active")
            .id,
        second.id
    );
    session_complete_tx(&mut tx, &second.id, WorkerSessionState::Exited)
        .await
        .unwrap();
    assert!(
        session_projection_active_for_card_tx(&mut tx, card.id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn runtime_get_active_by_thread_finds_active() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut init = runtime_init(
        card.id.to_string(),
        WorkerSessionKind::CodexCard,
        Some(AgentProvider::Codex),
        WorkerSessionState::Running,
    );
    init.thread_id = Some("thread-active".into());
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_start_runtime_tx(&mut tx, init).await.unwrap();
    tx.commit().await.unwrap();

    let found = repo
        .session_projection_active_by_thread(AgentProvider::Codex, "thread-active")
        .await
        .unwrap()
        .expect("active runtime by thread");
    assert_eq!(found.id, runtime.id);
    assert_eq!(found.card_id, card.id.to_string());
}

#[tokio::test]
async fn runtime_get_active_by_thread_skips_terminal_status() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut init = runtime_init(
        card.id.to_string(),
        WorkerSessionKind::CodexCard,
        Some(AgentProvider::Codex),
        WorkerSessionState::Running,
    );
    init.thread_id = Some("thread-complete".into());
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_start_runtime_tx(&mut tx, init).await.unwrap();
    session_complete_tx(&mut tx, &runtime.id, WorkerSessionState::Exited)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert!(
        repo.session_projection_active_by_thread(AgentProvider::Codex, "thread-complete")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn runtime_active_shared_thread_attribution_returns_shared_and_codex_with_thread() {
    let repo = fresh_repo().await;
    let shared = make_card(&repo, "codex").await;
    let codex = make_card(&repo, "codex").await;
    let no_thread = make_card(&repo, "codex").await;
    let claude = make_card(&repo, "claude").await;

    let mut shared_init = runtime_init(
        shared.id.to_string(),
        WorkerSessionKind::SharedSpec,
        Some(AgentProvider::Codex),
        WorkerSessionState::Running,
    );
    shared_init.thread_id = Some("thread-shared".into());
    let mut codex_init = runtime_init(
        codex.id.to_string(),
        WorkerSessionKind::CodexCard,
        Some(AgentProvider::Codex),
        WorkerSessionState::Running,
    );
    codex_init.thread_id = Some("thread-codex".into());
    let no_thread_init = runtime_init(
        no_thread.id.to_string(),
        WorkerSessionKind::CodexCard,
        Some(AgentProvider::Codex),
        WorkerSessionState::Running,
    );
    let mut claude_init = runtime_init(
        claude.id.to_string(),
        WorkerSessionKind::ClaudeCard,
        Some(AgentProvider::Claude),
        WorkerSessionState::Running,
    );
    claude_init.thread_id = Some("thread-claude".into());

    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(&mut tx, shared_init)
        .await
        .unwrap();
    session_start_runtime_tx(&mut tx, codex_init).await.unwrap();
    session_start_runtime_tx(&mut tx, no_thread_init)
        .await
        .unwrap();
    session_start_runtime_tx(&mut tx, claude_init)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut rows = repo
        .session_projection_active_shared_thread_attribution()
        .await
        .unwrap();
    rows.sort();
    assert_eq!(
        rows,
        vec![
            ("thread-codex".to_string(), codex.id.to_string()),
            ("thread-shared".to_string(), shared.id.to_string()),
        ]
    );
}

#[tokio::test]
async fn runtimes_active_for_kind_filters() {
    let repo = fresh_repo().await;
    let active_shared = make_card(&repo, "codex").await;
    let active_codex = make_card(&repo, "codex").await;
    let completed_shared = make_card(&repo, "codex").await;

    let mut tx = repo.pool().begin().await.unwrap();
    let active_shared_runtime = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            active_shared.id.to_string(),
            WorkerSessionKind::SharedSpec,
            Some(AgentProvider::Codex),
            WorkerSessionState::Running,
        ),
    )
    .await
    .unwrap();
    session_start_runtime_tx(
        &mut tx,
        runtime_init(
            active_codex.id.to_string(),
            WorkerSessionKind::CodexCard,
            Some(AgentProvider::Codex),
            WorkerSessionState::Running,
        ),
    )
    .await
    .unwrap();
    let completed = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            completed_shared.id.to_string(),
            WorkerSessionKind::SharedSpec,
            Some(AgentProvider::Codex),
            WorkerSessionState::Running,
        ),
    )
    .await
    .unwrap();
    session_complete_tx(&mut tx, &completed.id, WorkerSessionState::Failed)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let rows = repo
        .session_projection_active_for_kind(WorkerSessionKind::SharedSpec)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, active_shared_runtime.id);
    assert_eq!(rows[0].kind, WorkerSessionKind::SharedSpec);
}

#[tokio::test]
async fn runtimes_active_for_kind_codex_kind_excludes_placeholder() {
    let repo = fresh_repo().await;
    let placeholder_card = make_card(&repo, "codex").await;
    let codex_card = make_card(&repo, "codex").await;

    let placeholder = runtime_init(
        placeholder_card.id.to_string(),
        WorkerSessionKind::SharedSpec,
        Some(AgentProvider::Codex),
        WorkerSessionState::Starting,
    );
    let codex = runtime_init(
        codex_card.id.to_string(),
        WorkerSessionKind::CodexCard,
        Some(AgentProvider::Codex),
        WorkerSessionState::Running,
    );
    let placeholder_id = placeholder.id.clone();
    let codex_id = codex.id.clone();

    let mut tx = repo.pool().begin().await.unwrap();
    session_prepare_deferred_spec_tx(&mut tx, &placeholder)
        .await
        .unwrap();
    session_start_runtime_tx(&mut tx, codex).await.unwrap();
    tx.commit().await.unwrap();

    let rows = repo
        .session_projection_active_for_kind(WorkerSessionKind::CodexCard)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, codex_id);
    assert_ne!(rows[0].id, placeholder_id);
}
