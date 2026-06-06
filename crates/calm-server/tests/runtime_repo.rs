use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_with_claude_create_tx, card_with_codex_create_tx, card_with_terminal_create_tx,
    runtime_bind_attribution_tx, runtime_complete_for_card_tx, runtime_complete_tx,
    runtime_get_active_for_card_tx, runtime_get_by_id_tx, runtime_set_status_for_card_tx,
    runtime_set_status_tx, runtime_start_tx, runtime_supersede_tx,
};
use calm_server::model::{Card, CardRole, NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::runtime_repo::{
    AgentProvider, RunStatus, RuntimeInit, RuntimeKind, RuntimeRepoError, ThreadAttribution,
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
        attach_folder: false,
        theme: calm_server::routes::theme::RequestTheme::default_dark(),
    })
    .await
    .expect("create wave")
}

async fn make_card(repo: &SqlxRepo, kind: &str) -> Card {
    let wave = make_wave(repo).await;
    repo.card_create(NewCard {
        wave_id: wave.id,
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

#[tokio::test]
async fn runtime_start_tx_terminal_persists_active_row() {
    let repo = fresh_repo().await;
    let wave = make_wave(&repo).await;
    let mut tx = repo.pool().begin().await.unwrap();
    let (card, term) = card_with_terminal_create_tx(
        &mut tx,
        new_id(),
        wave.id,
        None,
        "bash".into(),
        "/tmp".into(),
        json!({}),
        CardRole::Plain,
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
        wave.id,
        None,
        "bash".into(),
        "/tmp".into(),
        json!({}),
        CardRole::Plain,
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
        wave.id,
        None,
        "bash".into(),
        "/tmp".into(),
        json!({}),
        CardRole::Plain,
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
        wave.id,
        None,
        "bash".into(),
        "/tmp".into(),
        json!({}),
        CardRole::Plain,
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
        wave.id,
        None,
        "bash".into(),
        "/tmp".into(),
        json!({}),
        CardRole::Plain,
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
        wave.id,
        None,
        "bash".into(),
        "/tmp".into(),
        json!({}),
        CardRole::Plain,
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
        wave.id,
        None,
        "/workspace".into(),
        json!({"CODEX_HOME": "/tmp/codex-home"}),
        None,
        None,
        None,
        CardRole::Plain,
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
