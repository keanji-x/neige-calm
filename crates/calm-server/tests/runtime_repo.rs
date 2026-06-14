mod support;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_with_claude_create_tx, card_with_codex_create_tx, card_with_terminal_create_tx,
    runtime_bind_attribution_tx, runtime_complete_for_card_tx, runtime_complete_tx,
    runtime_fail_if_active_tx, runtime_get_active_for_card_tx, runtime_get_by_id_tx,
    runtime_mark_superseded_tx, runtime_restore_from_superseded_tx, runtime_set_active_turn_tx,
    runtime_set_handle_state_tx, runtime_set_harness_observation_tx,
    runtime_set_status_for_card_tx, runtime_set_status_tx, runtime_start_tx, runtime_supersede_tx,
    session_insert_tx, session_mcp_token_set_tx, session_prepare_deferred_spec_tx,
};
use calm_server::model::{Card, CardRole, NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::runtime_lookup::project_runtime_into_card_payload;
use calm_server::runtime_repo::{
    AgentProvider, RunStatus, RuntimeInit, RuntimeKind, RuntimeRepo, RuntimeRepoError,
    ThreadAttribution,
};
use calm_types::worker::{
    LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession, WorkerSessionId,
    WorkerSessionState,
};
use serde_json::json;
use support::parity::assert_runtimes_worker_sessions_parity;

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
    kind: RuntimeKind,
    agent_provider: Option<AgentProvider>,
    status: RunStatus,
) -> RuntimeInit {
    RuntimeInit {
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
        lease_owner: None,
        lease_until_ms: None,
        now_ms: now_ms(),
    }
}

async fn runtime_row_snapshot(repo: &SqlxRepo, runtime_id: &str) -> (String, i64, Option<i64>) {
    sqlx::query_as(
        r#"SELECT status, updated_at_ms, completed_at_ms
           FROM runtimes
           WHERE id = ?1"#,
    )
    .bind(runtime_id)
    .fetch_one(repo.pool())
    .await
    .expect("runtime row snapshot")
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
        handle_state_json: Some(json!({"mode": "harness"})),
        liveness: LivenessTag::Unknown,
        liveness_probed_at_ms: None,
        exit_code: None,
        exit_interpretation: None,
        spawn_op_id: None,
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
    let first = runtime_start_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::SharedSpec,
            Some(AgentProvider::Codex),
            RunStatus::Starting,
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
    let second = runtime_supersede_tx(
        &mut tx,
        &first.id,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::SharedSpec,
            Some(AgentProvider::Codex),
            RunStatus::Starting,
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
    let failed = runtime_start_tx(
        &mut tx,
        runtime_init(
            failed_card.id.to_string(),
            RuntimeKind::SharedSpec,
            Some(AgentProvider::Codex),
            RunStatus::Failed,
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
    let live = runtime_start_tx(
        &mut tx,
        runtime_init(
            live_card.id.to_string(),
            RuntimeKind::SharedSpec,
            Some(AgentProvider::Codex),
            RunStatus::Starting,
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
    let exited = runtime_start_tx(
        &mut tx,
        runtime_init(
            exited_card.id.to_string(),
            RuntimeKind::SharedSpec,
            Some(AgentProvider::Codex),
            RunStatus::Exited,
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
    let root = runtime_start_tx(
        &mut tx,
        runtime_init(
            planner_card.id.to_string(),
            RuntimeKind::SharedSpec,
            Some(AgentProvider::Codex),
            RunStatus::Starting,
        ),
    )
    .await
    .unwrap();
    let executor = runtime_start_tx(
        &mut tx,
        runtime_init(
            executor_card.id.to_string(),
            RuntimeKind::CodexCard,
            Some(AgentProvider::Codex),
            RunStatus::Starting,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = repo.pool().begin().await.unwrap();
    let replacement = runtime_supersede_tx(
        &mut tx,
        &executor.id,
        runtime_init(
            executor_card.id.to_string(),
            RuntimeKind::CodexCard,
            Some(AgentProvider::Codex),
            RunStatus::Starting,
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
    let runtime = runtime_start_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::CodexCard,
            Some(AgentProvider::Codex),
            RunStatus::Starting,
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
async fn runtime_restore_repoints_card_and_root_to_restored_session() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let old_hash = "old-restored-token-hash";
    let replacement_hash = "replacement-failed-token-hash";

    let mut tx = repo.pool().begin().await.unwrap();
    let old = runtime_start_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::SharedSpec,
            Some(AgentProvider::Codex),
            RunStatus::Idle,
        ),
    )
    .await
    .unwrap();
    session_mcp_token_set_tx(&mut tx, &old.id, old_hash)
        .await
        .unwrap();
    let replacement = runtime_supersede_tx(
        &mut tx,
        &old.id,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::SharedSpec,
            Some(AgentProvider::Codex),
            RunStatus::Starting,
        ),
    )
    .await
    .unwrap();
    session_mcp_token_set_tx(&mut tx, &replacement.id, replacement_hash)
        .await
        .unwrap();
    runtime_fail_if_active_tx(&mut tx, &replacement.id)
        .await
        .unwrap();
    runtime_restore_from_superseded_tx(&mut tx, &old.id, RunStatus::Idle)
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
        .runtime_get_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("restored old runtime should be active");
    assert_eq!(restored.id, old.id);
    assert_eq!(restored.status, RunStatus::Idle);
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
async fn deferred_spec_placeholder_does_not_claim_current_links() {
    let repo = fresh_repo().await;
    let active_card = make_card(&repo, "codex").await;

    let mut tx = repo.pool().begin().await.unwrap();
    let old = runtime_start_tx(
        &mut tx,
        runtime_init(
            active_card.id.to_string(),
            RuntimeKind::SharedSpec,
            Some(AgentProvider::Codex),
            RunStatus::Idle,
        ),
    )
    .await
    .unwrap();
    let placeholder_init = runtime_init(
        active_card.id.to_string(),
        RuntimeKind::SharedSpec,
        Some(AgentProvider::Codex),
        RunStatus::Starting,
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
    assert_eq!(active_link.as_deref(), Some(old.id.as_str()));
    assert_ne!(active_link.as_deref(), Some(placeholder_id.as_str()));

    let active_root: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM waves WHERE id = ?1")
            .bind(active_card.wave_id.as_str())
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(active_root.as_deref(), Some(old.id.as_str()));
    assert_ne!(active_root.as_deref(), Some(placeholder_id.as_str()));

    let fresh_card = make_card(&repo, "codex").await;
    let fresh_placeholder_init = runtime_init(
        fresh_card.id.to_string(),
        RuntimeKind::SharedSpec,
        Some(AgentProvider::Codex),
        RunStatus::Starting,
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
    assert_eq!(fresh_link, None);
    let fresh_root: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM waves WHERE id = ?1")
            .bind(fresh_card.wave_id.as_str())
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(fresh_root, None);
    assert!(
        repo.session_get(&WorkerSessionId::from(fresh_placeholder_id))
            .await
            .unwrap()
            .is_some(),
        "fresh deferred placeholder session should still exist for parity"
    );
}

#[tokio::test]
async fn runtime_entrances_dual_write_worker_session_parity() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = runtime_start_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::CodexCard,
            Some(AgentProvider::Codex),
            RunStatus::Starting,
        ),
    )
    .await
    .unwrap();
    runtime_bind_attribution_tx(
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
    runtime_set_status_tx(&mut tx, &runtime.id, RunStatus::Running)
        .await
        .unwrap();
    runtime_set_active_turn_tx(&mut tx, &runtime.id, Some("turn-2"))
        .await
        .unwrap();
    runtime_set_handle_state_tx(
        &mut tx,
        &runtime.id,
        Some(json!({"phase": "dual-write", "n": 1})),
    )
    .await
    .unwrap();
    runtime_complete_tx(&mut tx, &runtime.id, RunStatus::Exited)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_runtimes_worker_sessions_parity(repo.pool()).await;
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
    let runtime = runtime_start_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::SharedSpec,
            Some(AgentProvider::Codex),
            RunStatus::Idle,
        ),
    )
    .await
    .unwrap();
    runtime_set_harness_observation_tx(
        &mut tx,
        &runtime.id,
        RunStatus::TurnPending,
        Some("thread-harness"),
        Some("turn-harness"),
    )
    .await
    .unwrap();
    runtime_fail_if_active_tx(&mut tx, &runtime.id)
        .await
        .unwrap();
    runtime_mark_superseded_tx(&mut tx, &runtime.id)
        .await
        .unwrap();
    runtime_restore_from_superseded_tx(&mut tx, &runtime.id, RunStatus::Running)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_runtimes_worker_sessions_parity(repo.pool()).await;
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
async fn backfill_worker_sessions_from_runtimes_is_idempotent() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let runtime_id = new_id();
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO runtimes
           (id, card_id, kind, agent_provider, status, thread_id, session_id, active_turn_id,
            created_at_ms, updated_at_ms)
           VALUES (?1, ?2, 'codex', 'codex', 'running', 'thread-backfill',
                   'agent-session-backfill', 'turn-backfill', ?3, ?3)"#,
    )
    .bind(&runtime_id)
    .bind(card.id.as_str())
    .bind(now)
    .execute(repo.pool())
    .await
    .unwrap();

    assert_eq!(
        repo.backfill_worker_sessions_from_runtimes().await.unwrap(),
        1
    );
    assert_eq!(
        repo.backfill_worker_sessions_from_runtimes().await.unwrap(),
        0
    );
    assert_runtimes_worker_sessions_parity(repo.pool()).await;

    let session = repo
        .session_get(&WorkerSessionId(runtime_id))
        .await
        .unwrap()
        .expect("backfilled worker session");
    assert_eq!(session.state, WorkerSessionState::Running);
    assert_eq!(session.thread_id.as_deref(), Some("thread-backfill"));
    assert_eq!(
        session.agent_session_id.as_deref(),
        Some("agent-session-backfill")
    );
    assert_eq!(session.active_turn_id.as_deref(), Some("turn-backfill"));
}

#[tokio::test]
async fn runtime_supersede_tx_mirrors_old_superseded_and_new_starting_same_wave() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let first = runtime_start_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::CodexCard,
            Some(AgentProvider::Codex),
            RunStatus::Starting,
        ),
    )
    .await
    .unwrap();
    let second = runtime_supersede_tx(
        &mut tx,
        &first.id,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::CodexCard,
            Some(AgentProvider::Codex),
            RunStatus::Starting,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    assert_runtimes_worker_sessions_parity(repo.pool()).await;
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
    let old = runtime_start_tx(
        &mut tx,
        runtime_init(
            card_id.clone(),
            RuntimeKind::SharedSpec,
            Some(AgentProvider::Codex),
            RunStatus::Idle,
        ),
    )
    .await
    .unwrap();
    runtime_mark_superseded_tx(&mut tx, &old.id).await.unwrap();

    runtime_set_harness_observation_tx(
        &mut tx,
        &old.id,
        RunStatus::Running,
        Some("stale-thread"),
        Some("stale-turn"),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let old_after = repo
        .runtime_get_by_id(&old.id)
        .await
        .unwrap()
        .expect("old runtime");
    assert_eq!(old_after.status, RunStatus::Superseded);
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

    let active = repo.runtime_get_active_for_card(&card_id).await.unwrap();
    assert!(active.is_none());

    let active_count: (i64,) = sqlx::query_as(
        r#"SELECT COUNT(*)
           FROM runtimes
          WHERE card_id = ?1
            AND status IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(card_id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(active_count.0, 0);
    assert_runtimes_worker_sessions_parity(repo.pool()).await;

    let replacement_card = make_card(&repo, "spec").await;
    let replacement_card_id = replacement_card.id.to_string();
    let mut tx = repo.pool().begin().await.unwrap();
    let replaced_old = runtime_start_tx(
        &mut tx,
        runtime_init(
            replacement_card_id.clone(),
            RuntimeKind::SharedSpec,
            Some(AgentProvider::Codex),
            RunStatus::Idle,
        ),
    )
    .await
    .unwrap();
    let replacement = runtime_supersede_tx(
        &mut tx,
        &replaced_old.id,
        runtime_init(
            replacement_card_id.clone(),
            RuntimeKind::SharedSpec,
            Some(AgentProvider::Codex),
            RunStatus::Starting,
        ),
    )
    .await
    .unwrap();

    runtime_set_harness_observation_tx(
        &mut tx,
        &replaced_old.id,
        RunStatus::Running,
        Some("stale-replaced-thread"),
        Some("stale-replaced-turn"),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let replaced_old_after = repo
        .runtime_get_by_id(&replaced_old.id)
        .await
        .unwrap()
        .expect("replaced old runtime");
    assert_eq!(replaced_old_after.status, RunStatus::Superseded);
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
        .runtime_get_active_for_card(&replacement_card_id)
        .await
        .unwrap()
        .expect("replacement active runtime");
    assert_eq!(replacement_active.id, replacement.id);

    let replacement_active_count: (i64,) = sqlx::query_as(
        r#"SELECT COUNT(*)
           FROM runtimes
          WHERE card_id = ?1
            AND status IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(replacement_card_id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(replacement_active_count.0, 1);
    assert_runtimes_worker_sessions_parity(repo.pool()).await;
}

#[tokio::test]
async fn runtime_start_tx_terminal_persists_active_row() {
    let repo = fresh_repo().await;
    let wave = make_wave(&repo).await;
    let mut tx = repo.pool().begin().await.unwrap();
    let (card, term) = card_with_terminal_create_tx(
        &mut tx,
        new_id(),
        &new_id(),
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

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM runtimes WHERE card_id = ?1")
        .bind(card.id.as_str())
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(count.0, 1);

    let active = repo
        .runtime_get_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("active runtime");
    assert_eq!(active.kind, RuntimeKind::Terminal);
    assert_eq!(active.status, RunStatus::Starting);
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
        .runtime_get_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("active runtime");
    repo.runtime_complete_for_terminal(&term.id, RunStatus::Exited)
        .await
        .unwrap();

    let completed = repo
        .runtime_get_by_id(&active.id)
        .await
        .unwrap()
        .expect("runtime");
    assert_eq!(completed.kind, RuntimeKind::Terminal);
    assert_eq!(completed.status, RunStatus::Exited);
    assert_eq!(completed.terminal_run_id.as_deref(), Some(term.id.as_str()));
    assert!(completed.completed_at_ms.is_some());
    assert!(
        repo.runtime_get_active_for_card(&card.id.to_string())
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
        .runtime_get_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("active runtime");
    repo.runtime_complete_for_terminal(&term.id, RunStatus::Failed)
        .await
        .unwrap();

    let completed = repo
        .runtime_get_by_id(&active.id)
        .await
        .unwrap()
        .expect("runtime");
    assert_eq!(completed.status, RunStatus::Failed);
    assert_eq!(completed.terminal_run_id.as_deref(), Some(term.id.as_str()));
    assert!(completed.completed_at_ms.is_some());
}

#[tokio::test]
async fn runtime_complete_for_terminal_noop_when_no_active() {
    let repo = fresh_repo().await;
    repo.runtime_complete_for_terminal("missing-terminal", RunStatus::Exited)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runtimes")
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
    let runtime_id = runtime_get_active_for_card_tx(&mut tx, card.id.as_ref())
        .await
        .unwrap()
        .expect("active runtime")
        .id;
    runtime_complete_tx(&mut tx, &runtime_id, RunStatus::Exited)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let before = runtime_row_snapshot(&repo, &runtime_id).await;
    let mut tx = repo.pool().begin().await.unwrap();
    runtime_set_status_for_card_tx(&mut tx, card.id.as_ref(), RunStatus::Running)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let after = runtime_row_snapshot(&repo, &runtime_id).await;

    assert_eq!(before, after);
    assert_eq!(after.0, "exited");
    assert!(
        repo.runtime_get_active_for_card(&card.id.to_string())
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
    let runtime_id = runtime_get_active_for_card_tx(&mut tx, card.id.as_ref())
        .await
        .unwrap()
        .expect("active runtime")
        .id;
    runtime_complete_tx(&mut tx, &runtime_id, RunStatus::Exited)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let before = runtime_row_snapshot(&repo, &runtime_id).await;
    let mut tx = repo.pool().begin().await.unwrap();
    runtime_complete_for_card_tx(&mut tx, card.id.as_ref(), RunStatus::Failed)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let after = runtime_row_snapshot(&repo, &runtime_id).await;

    assert_eq!(before, after);
    assert_eq!(after.0, "exited");
    assert!(
        repo.runtime_get_active_for_card(&card.id.to_string())
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
    let runtime_id = runtime_get_active_for_card_tx(&mut tx, card.id.as_ref())
        .await
        .unwrap()
        .expect("active runtime")
        .id;
    runtime_set_status_for_card_tx(&mut tx, card.id.as_ref(), RunStatus::Running)
        .await
        .unwrap();
    runtime_complete_for_card_tx(&mut tx, card.id.as_ref(), RunStatus::Failed)
        .await
        .unwrap();
    let completed = runtime_get_by_id_tx(&mut tx, &runtime_id)
        .await
        .unwrap()
        .expect("completed runtime");
    let active_after_complete = runtime_get_active_for_card_tx(&mut tx, card.id.as_ref())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(completed.status, RunStatus::Failed);
    assert!(completed.completed_at_ms.is_some());
    assert!(active_after_complete.is_none());
    let row: (String, Option<i64>) =
        sqlx::query_as("SELECT status, completed_at_ms FROM runtimes WHERE card_id = ?1")
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
        .runtime_get_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("active runtime");
    assert_eq!(active.kind, RuntimeKind::CodexCard);
    assert_eq!(active.status, RunStatus::Starting);
    assert_eq!(active.terminal_run_id.as_deref(), Some(term.id.as_str()));
    assert!(active.thread_id.is_none());
}

#[tokio::test]
async fn runtime_one_active_per_card_invariant_enforced() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();

    runtime_start_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::CodexCard,
            Some(AgentProvider::Codex),
            RunStatus::Running,
        ),
    )
    .await
    .unwrap();
    let err = runtime_start_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::CodexCard,
            Some(AgentProvider::Codex),
            RunStatus::Running,
        ),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err,
        RuntimeRepoError::Message { message }
            if message.contains("runtimes.card_id") || message.contains("UNIQUE")
    ));
}

async fn insert_raw_runtime(
    repo: &SqlxRepo,
    card_id: &str,
    kind: &str,
    agent_provider: Option<&str>,
) -> Result<sqlx::sqlite::SqliteQueryResult, sqlx::Error> {
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO runtimes
           (id, card_id, kind, agent_provider, status, created_at_ms, updated_at_ms)
           VALUES (?1, ?2, ?3, ?4, 'exited', ?5, ?6)"#,
    )
    .bind(new_id())
    .bind(card_id)
    .bind(kind)
    .bind(agent_provider)
    .bind(now)
    .bind(now)
    .execute(repo.pool())
    .await
}

#[tokio::test]
async fn runtime_check_rejects_null_agent_provider_for_non_terminal_kinds() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;

    for kind in ["codex", "claude", "shared-spec"] {
        let err = insert_raw_runtime(&repo, card.id.as_str(), kind, None)
            .await
            .unwrap_err();
        let sqlx::Error::Database(db_err) = err else {
            panic!("expected database error for {kind}");
        };
        assert!(
            db_err.message().to_ascii_uppercase().contains("CHECK"),
            "expected CHECK constraint error for {kind}, got: {}",
            db_err.message()
        );
    }

    insert_raw_runtime(&repo, card.id.as_str(), "terminal", None)
        .await
        .expect("terminal runtime with null agent_provider should pass");
    assert_eq!(
        repo.backfill_worker_sessions_from_runtimes().await.unwrap(),
        1,
        "raw terminal runtime inserted for CHECK coverage is backfilled before teardown parity"
    );
}

#[tokio::test]
async fn runtime_supersede_tx_atomic() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let first = runtime_start_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::CodexCard,
            Some(AgentProvider::Codex),
            RunStatus::Starting,
        ),
    )
    .await
    .unwrap();
    let second_init = runtime_init(
        card.id.to_string(),
        RuntimeKind::CodexCard,
        Some(AgentProvider::Codex),
        RunStatus::Running,
    );
    let second = runtime_supersede_tx(&mut tx, &first.id, second_init)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let old = repo
        .runtime_get_by_id(&first.id)
        .await
        .unwrap()
        .expect("old runtime");
    assert_eq!(old.status, RunStatus::Superseded);
    assert_eq!(second.status, RunStatus::Running);

    let active_count: (i64,) = sqlx::query_as(
        r#"SELECT COUNT(*) FROM runtimes
           WHERE card_id = ?1
             AND status IN ('starting', 'running', 'idle', 'turn_pending')"#,
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
    let runtime = runtime_start_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::CodexCard,
            Some(AgentProvider::Codex),
            RunStatus::Running,
        ),
    )
    .await
    .unwrap();

    let err = runtime_set_status_tx(&mut tx, &runtime.id, RunStatus::Superseded)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        RuntimeRepoError::IllegalStatusTransition {
            attempted: RunStatus::Superseded,
            ..
        }
    ));
}

#[tokio::test]
async fn runtime_set_status_same_running_rejected() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = runtime_start_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::CodexCard,
            Some(AgentProvider::Codex),
            RunStatus::Running,
        ),
    )
    .await
    .unwrap();

    let err = runtime_set_status_tx(&mut tx, &runtime.id, RunStatus::Running)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        RuntimeRepoError::IllegalStatusTransition {
            attempted: RunStatus::Running,
            ..
        }
    ));
}

#[tokio::test]
async fn runtime_bind_attribution_transitions_pending_to_running() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = runtime_start_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::CodexCard,
            Some(AgentProvider::Codex),
            RunStatus::TurnPending,
        ),
    )
    .await
    .unwrap();
    runtime_bind_attribution_tx(
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
    runtime_set_status_tx(&mut tx, &runtime.id, RunStatus::Running)
        .await
        .unwrap();
    let persisted = runtime_get_by_id_tx(&mut tx, &runtime.id)
        .await
        .unwrap()
        .expect("runtime");
    tx.commit().await.unwrap();

    assert_eq!(persisted.status, RunStatus::Running);
    assert_eq!(persisted.thread_id.as_deref(), Some("thread-pending-bind"));
}

#[tokio::test]
async fn runtime_start_tx_codex_empty_is_turn_pending() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = runtime_start_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::CodexCard,
            Some(AgentProvider::Codex),
            RunStatus::TurnPending,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let persisted = repo
        .runtime_get_by_id(&runtime.id)
        .await
        .unwrap()
        .expect("runtime");
    assert_eq!(persisted.status, RunStatus::TurnPending);
    assert!(persisted.thread_id.is_none());
}

#[tokio::test]
async fn runtime_pending_drop_completes_failed() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = runtime_start_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::CodexCard,
            Some(AgentProvider::Codex),
            RunStatus::TurnPending,
        ),
    )
    .await
    .unwrap();
    runtime_complete_tx(&mut tx, &runtime.id, RunStatus::Failed)
        .await
        .unwrap();
    let completed = runtime_get_by_id_tx(&mut tx, &runtime.id)
        .await
        .unwrap()
        .expect("runtime");
    tx.commit().await.unwrap();

    assert_eq!(completed.status, RunStatus::Failed);
    assert!(completed.completed_at_ms.is_some());
}

#[tokio::test]
async fn runtime_start_tx_claude_records_session_when_present() {
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
        .runtime_get_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("active runtime");
    assert_eq!(active.kind, RuntimeKind::ClaudeCard);
    assert_eq!(active.status, RunStatus::Starting);
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
    assert_eq!(runtime.kind, RuntimeKind::ClaudeCard);
    assert_eq!(runtime.status, RunStatus::Starting);
    assert_eq!(runtime.provider, Some(AgentProvider::Claude));
    assert_eq!(runtime.terminal_id.as_deref(), Some(term.id.as_str()));
    assert_eq!(runtime.session_id.as_deref(), Some(session_id.as_str()));
    assert!(runtime.thread_id.is_none());
    assert!(runtime.source.is_none());
    assert!(runtime.thread_status.is_none());
    assert_eq!(stored.payload["terminal_id"], term.id);
    assert_eq!(stored.payload["claude_session_id"], session_id);

    assert_runtimes_worker_sessions_parity(repo.pool()).await;
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
        RuntimeKind::CodexCard,
        Some(AgentProvider::Codex),
        RunStatus::Running,
    );
    init.handle_state_json = Some(state.clone());

    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = runtime_start_tx(&mut tx, init).await.unwrap();
    tx.commit().await.unwrap();

    let persisted = repo
        .runtime_get_by_id(&runtime.id)
        .await
        .unwrap()
        .expect("runtime");
    assert_eq!(persisted.handle_state_json, Some(state));
}

#[tokio::test]
async fn runtime_set_handle_state_tx_writes_active_runtime() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let state = json!({"phase": "active-write", "queue": [1, 2, 3]});

    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = runtime_start_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::CodexCard,
            Some(AgentProvider::Codex),
            RunStatus::Running,
        ),
    )
    .await
    .unwrap();
    runtime_set_handle_state_tx(&mut tx, &runtime.id, Some(state.clone()))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let persisted = repo
        .runtime_get_by_id(&runtime.id)
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
async fn runtime_set_handle_state_tx_noops_for_superseded_runtime() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let original = json!({"phase": "original", "queue": [1]});
    let stale = json!({"phase": "stale", "queue": [2]});
    let mut init = runtime_init(
        card.id.to_string(),
        RuntimeKind::CodexCard,
        Some(AgentProvider::Codex),
        RunStatus::Running,
    );
    init.handle_state_json = Some(original.clone());

    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = runtime_start_tx(&mut tx, init).await.unwrap();
    let _replacement = runtime_supersede_tx(
        &mut tx,
        &runtime.id,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::CodexCard,
            Some(AgentProvider::Codex),
            RunStatus::Starting,
        ),
    )
    .await
    .unwrap();
    runtime_set_handle_state_tx(&mut tx, &runtime.id, Some(stale))
        .await
        .expect("superseded handle-state write should no-op");
    tx.commit().await.unwrap();

    let stale_runtime = repo
        .runtime_get_by_id(&runtime.id)
        .await
        .unwrap()
        .expect("superseded runtime");
    assert_eq!(stale_runtime.status, RunStatus::Superseded);
    assert_eq!(stale_runtime.handle_state_json, Some(original.clone()));

    let stale_session = repo
        .session_get(&WorkerSessionId(runtime.id))
        .await
        .unwrap()
        .expect("mirrored superseded worker session");
    assert_eq!(stale_session.state, WorkerSessionState::Superseded);
    assert_eq!(stale_session.handle_state_json, Some(original));
    assert_runtimes_worker_sessions_parity(repo.pool()).await;
}

#[tokio::test]
async fn runtimes_recover_orphans_skips_unleased_runtimes() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "terminal").await;
    let mut init = runtime_init(
        card.id.to_string(),
        RuntimeKind::Terminal,
        None,
        RunStatus::Running,
    );
    init.now_ms = now_ms() - 120_000;

    let mut tx = repo.pool().begin().await.unwrap();
    runtime_start_tx(&mut tx, init).await.unwrap();
    tx.commit().await.unwrap();

    let recovered = repo.runtimes_recover_orphans_on_boot().await.unwrap();
    assert!(recovered.is_empty());
}

#[tokio::test]
async fn runtimes_recover_orphans_finds_expired_lease() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let now = now_ms();
    let mut init = runtime_init(
        card.id.to_string(),
        RuntimeKind::CodexCard,
        Some(AgentProvider::Codex),
        RunStatus::Running,
    );
    init.lease_owner = Some("test".into());
    init.lease_until_ms = Some(now - 1_000);
    init.now_ms = now - 120_000;

    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = runtime_start_tx(&mut tx, init).await.unwrap();
    tx.commit().await.unwrap();

    let recovered = repo.runtimes_recover_orphans_on_boot().await.unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].id, runtime.id);
}

#[tokio::test]
async fn runtimes_recover_orphans_skips_active_lease() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "claude").await;
    let now = now_ms();
    let mut init = runtime_init(
        card.id.to_string(),
        RuntimeKind::ClaudeCard,
        Some(AgentProvider::Claude),
        RunStatus::Running,
    );
    init.lease_owner = Some("test".into());
    init.lease_until_ms = Some(now + 60_000);
    init.now_ms = now - 120_000;

    let mut tx = repo.pool().begin().await.unwrap();
    runtime_start_tx(&mut tx, init).await.unwrap();
    tx.commit().await.unwrap();

    let recovered = repo.runtimes_recover_orphans_on_boot().await.unwrap();
    assert!(recovered.is_empty());
}

#[tokio::test]
async fn runtime_start_tx_shared_spec_thread_present_running() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut init = runtime_init(
        card.id.to_string(),
        RuntimeKind::SharedSpec,
        Some(AgentProvider::Codex),
        RunStatus::Running,
    );
    init.thread_id = Some("thread-1".into());

    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = runtime_start_tx(&mut tx, init).await.unwrap();
    tx.commit().await.unwrap();

    assert_eq!(runtime.kind, RuntimeKind::SharedSpec);
    assert_eq!(runtime.status, RunStatus::Running);
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
        RuntimeKind::CodexCard,
        Some(AgentProvider::Codex),
        RunStatus::Running,
    );
    init.terminal_run_id = Some("NEW".into());
    init.thread_id = Some("abc".into());

    let mut tx = repo.pool().begin().await.unwrap();
    runtime_start_tx(&mut tx, init).await.unwrap();
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
    assert_eq!(runtime.kind, RuntimeKind::CodexCard);
    assert_eq!(runtime.status, RunStatus::Running);
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
        RuntimeKind::SharedSpec,
        Some(AgentProvider::Codex),
        RunStatus::Failed,
    );
    let mut active = runtime_init(
        card.id.to_string(),
        RuntimeKind::SharedSpec,
        Some(AgentProvider::Codex),
        RunStatus::Running,
    );
    active.thread_id = Some("active-thread".into());

    let mut tx = repo.pool().begin().await.unwrap();
    runtime_start_tx(&mut tx, failed).await.unwrap();
    runtime_start_tx(&mut tx, active).await.unwrap();
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
    assert_eq!(runtime.kind, RuntimeKind::SharedSpec);
    assert_eq!(runtime.status, RunStatus::Running);
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
        RuntimeKind::SharedSpec,
        Some(AgentProvider::Codex),
        RunStatus::Running,
    );
    first_init.thread_id = Some("T1".into());
    let mut second_init = runtime_init(
        card.id.to_string(),
        RuntimeKind::SharedSpec,
        Some(AgentProvider::Codex),
        RunStatus::Running,
    );
    second_init.thread_id = Some("T2".into());

    let mut tx = repo.pool().begin().await.unwrap();
    let first = runtime_start_tx(&mut tx, first_init).await.unwrap();
    let second = runtime_supersede_tx(&mut tx, &first.id, second_init)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let old = repo
        .runtime_get_by_id(&first.id)
        .await
        .unwrap()
        .expect("old runtime");
    assert_eq!(old.status, RunStatus::Superseded);
    assert_eq!(old.thread_id.as_deref(), Some("T1"));

    let active = repo
        .runtime_get_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("active runtime");
    assert_eq!(active.id, second.id);
    assert_eq!(active.kind, RuntimeKind::SharedSpec);
    assert_eq!(active.status, RunStatus::Running);
    assert_eq!(active.thread_id.as_deref(), Some("T2"));

    let active_count: (i64,) = sqlx::query_as(
        r#"SELECT COUNT(*) FROM runtimes
           WHERE card_id = ?1
             AND status IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(card.id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(active_count.0, 1);
}

#[tokio::test]
async fn runtime_start_tx_shared_spec_absent_turn_pending() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = runtime_start_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::SharedSpec,
            Some(AgentProvider::Codex),
            RunStatus::TurnPending,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(runtime.status, RunStatus::TurnPending);
    assert!(runtime.thread_id.is_none());
}

#[tokio::test]
async fn runtime_complete_tx_marks_completed_at() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = runtime_start_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::CodexCard,
            Some(AgentProvider::Codex),
            RunStatus::Running,
        ),
    )
    .await
    .unwrap();
    runtime_complete_tx(&mut tx, &runtime.id, RunStatus::Exited)
        .await
        .unwrap();
    let completed = runtime_get_by_id_tx(&mut tx, &runtime.id)
        .await
        .unwrap()
        .expect("runtime");
    tx.commit().await.unwrap();

    assert_eq!(completed.status, RunStatus::Exited);
    assert!(completed.completed_at_ms.is_some());
    assert!(completed.completed_at_ms.unwrap() >= completed.created_at_ms);
}

#[tokio::test]
async fn runtime_get_active_for_card_returns_none_when_only_superseded() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex").await;
    let mut tx = repo.pool().begin().await.unwrap();
    let first = runtime_start_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::CodexCard,
            Some(AgentProvider::Codex),
            RunStatus::Running,
        ),
    )
    .await
    .unwrap();
    let second = runtime_supersede_tx(
        &mut tx,
        &first.id,
        runtime_init(
            card.id.to_string(),
            RuntimeKind::CodexCard,
            Some(AgentProvider::Codex),
            RunStatus::Running,
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        runtime_get_active_for_card_tx(&mut tx, card.id.as_str())
            .await
            .unwrap()
            .expect("active")
            .id,
        second.id
    );
    runtime_complete_tx(&mut tx, &second.id, RunStatus::Exited)
        .await
        .unwrap();
    assert!(
        runtime_get_active_for_card_tx(&mut tx, card.id.as_str())
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
        RuntimeKind::CodexCard,
        Some(AgentProvider::Codex),
        RunStatus::Running,
    );
    init.thread_id = Some("thread-active".into());
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = runtime_start_tx(&mut tx, init).await.unwrap();
    tx.commit().await.unwrap();

    let found = repo
        .runtime_get_active_by_thread(AgentProvider::Codex, "thread-active")
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
        RuntimeKind::CodexCard,
        Some(AgentProvider::Codex),
        RunStatus::Running,
    );
    init.thread_id = Some("thread-complete".into());
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = runtime_start_tx(&mut tx, init).await.unwrap();
    runtime_complete_tx(&mut tx, &runtime.id, RunStatus::Exited)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert!(
        repo.runtime_get_active_by_thread(AgentProvider::Codex, "thread-complete")
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
        RuntimeKind::SharedSpec,
        Some(AgentProvider::Codex),
        RunStatus::Running,
    );
    shared_init.thread_id = Some("thread-shared".into());
    let mut codex_init = runtime_init(
        codex.id.to_string(),
        RuntimeKind::CodexCard,
        Some(AgentProvider::Codex),
        RunStatus::Running,
    );
    codex_init.thread_id = Some("thread-codex".into());
    let no_thread_init = runtime_init(
        no_thread.id.to_string(),
        RuntimeKind::CodexCard,
        Some(AgentProvider::Codex),
        RunStatus::Running,
    );
    let mut claude_init = runtime_init(
        claude.id.to_string(),
        RuntimeKind::ClaudeCard,
        Some(AgentProvider::Claude),
        RunStatus::Running,
    );
    claude_init.thread_id = Some("thread-claude".into());

    let mut tx = repo.pool().begin().await.unwrap();
    runtime_start_tx(&mut tx, shared_init).await.unwrap();
    runtime_start_tx(&mut tx, codex_init).await.unwrap();
    runtime_start_tx(&mut tx, no_thread_init).await.unwrap();
    runtime_start_tx(&mut tx, claude_init).await.unwrap();
    tx.commit().await.unwrap();

    let mut rows = repo
        .runtime_active_shared_thread_attribution()
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
    let active_shared_runtime = runtime_start_tx(
        &mut tx,
        runtime_init(
            active_shared.id.to_string(),
            RuntimeKind::SharedSpec,
            Some(AgentProvider::Codex),
            RunStatus::Running,
        ),
    )
    .await
    .unwrap();
    runtime_start_tx(
        &mut tx,
        runtime_init(
            active_codex.id.to_string(),
            RuntimeKind::CodexCard,
            Some(AgentProvider::Codex),
            RunStatus::Running,
        ),
    )
    .await
    .unwrap();
    let completed = runtime_start_tx(
        &mut tx,
        runtime_init(
            completed_shared.id.to_string(),
            RuntimeKind::SharedSpec,
            Some(AgentProvider::Codex),
            RunStatus::Running,
        ),
    )
    .await
    .unwrap();
    runtime_complete_tx(&mut tx, &completed.id, RunStatus::Failed)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let rows = repo
        .runtimes_active_for_kind(RuntimeKind::SharedSpec)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, active_shared_runtime.id);
    assert_eq!(rows[0].kind, RuntimeKind::SharedSpec);
}
