use super::session_projection::{
    runtime_active_shared_thread_attribution_from_pool, runtime_current_status_tx,
    runtime_get_active_by_session_from_pool, runtime_get_active_by_thread_from_pool,
    runtime_get_active_for_card_from_pool, runtime_get_by_id_from_pool,
    runtime_get_projectable_for_card_from_pool, runtime_get_projectable_for_cards_from_pool,
    runtimes_active_for_kind_from_pool,
};
use super::*;
use crate::db::RepoRead;
use crate::model::new_id;
use crate::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionProjectionRepoError,
};

use calm_types::worker::WorkerSessionState;

use super::runtime_read_flip_support::*;

#[tokio::test]
async fn runtime_active_shared_thread_attribution_from_pool_filters_and_orders_pairs() {
    let repo = fresh_repo().await;
    let shared = seed_runtime_with_keys(
        &repo,
        KeyedRuntimeSeed {
            label: "attribution-shared",
            card_kind: "codex",
            kind: WorkerSessionKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            thread_id: Some("thread-shared"),
            session_id: Some("session-shared"),
            now_ms: 10_000,
        },
    )
    .await;
    let codex = seed_runtime_with_keys(
        &repo,
        KeyedRuntimeSeed {
            label: "attribution-codex",
            card_kind: "codex",
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            thread_id: Some("thread-codex"),
            session_id: Some("session-codex"),
            now_ms: 20_000,
        },
    )
    .await;
    seed_runtime_with_keys(
        &repo,
        KeyedRuntimeSeed {
            label: "attribution-no-thread",
            card_kind: "codex",
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            thread_id: None,
            session_id: Some("session-no-thread"),
            now_ms: 30_000,
        },
    )
    .await;
    seed_runtime_with_keys(
        &repo,
        KeyedRuntimeSeed {
            label: "attribution-claude",
            card_kind: "claude",
            kind: WorkerSessionKind::ClaudeCard,
            agent_provider: Some(AgentProvider::Claude),
            thread_id: Some("thread-claude"),
            session_id: Some("session-claude"),
            now_ms: 15_000,
        },
    )
    .await;

    let actual = runtime_active_shared_thread_attribution_from_pool(repo.pool())
        .await
        .expect("worker-session attribution read");

    assert_eq!(
        actual,
        vec![
            ("thread-shared".to_string(), shared.card_id.clone()),
            ("thread-codex".to_string(), codex.card_id.clone()),
        ]
    );
}

#[tokio::test]
async fn runtime_get_active_for_terminal_tx_reads_active_terminal_session_inside_tx() {
    let repo = fresh_repo().await;
    let (runtime, terminal_id) = seed_terminal_runtime(&repo, "terminal-key").await;
    let mut tx = repo.pool().begin().await.expect("begin terminal read tx");
    let actual = session_projection_active_for_terminal_tx(&mut tx, &terminal_id)
        .await
        .expect("worker-session terminal read");
    assert_eq!(
        actual.as_ref().map(|runtime| runtime.id.as_str()),
        Some(runtime.id.as_str())
    );
    tx.commit().await.expect("commit terminal read tx");
}

#[tokio::test]
async fn runtime_get_projectable_for_card_from_pool_picks_active_winner_for_active_history() {
    let repo = fresh_repo().await;
    let history = seed_projectable_history(&repo, "projectable-active", true).await;
    let active = history.active.as_ref().expect("active runtime");

    assert_eq!(history.superseded.status, WorkerSessionState::Superseded);
    assert_eq!(history.exited.status, WorkerSessionState::Exited);
    assert_projectable_card_picks_active_winner(&repo, &history, &active.id).await;
}

#[tokio::test]
async fn runtime_get_projectable_for_card_from_pool_picks_exited_winner_without_active_history() {
    let repo = fresh_repo().await;
    let history = seed_projectable_history(&repo, "projectable-no-active", false).await;

    assert_eq!(history.superseded.status, WorkerSessionState::Superseded);
    assert!(history.active.is_none());
    assert_projectable_card_picks_active_winner(&repo, &history, &history.exited.id).await;
}

#[tokio::test]
async fn runtime_get_projectable_for_card_from_pool_returns_deferred_spec_placeholder() {
    let repo = fresh_repo().await;
    let (card_id, placeholder_id) =
        seed_deferred_projectable_placeholder(&repo, "projectable-placeholder").await;

    let actual = runtime_get_projectable_for_card_from_pool(repo.pool(), &card_id)
        .await
        .expect("worker-session projectable read")
        .expect("placeholder projectable read");
    assert_eq!(actual.id, placeholder_id);
}

// Phase 1 supersedes the old active worker session before inserting the
// deferred placeholder, so the card's projectable session is the
// placeholder until Phase 2 binds a real thread.
#[tokio::test]
async fn projectable_deferred_spec_gap_with_active_runtime_returns_placeholder() {
    let repo = fresh_repo().await;
    let label = "projectable-gap";
    let placeholder_id = format!("rt-projectable-placeholder-{label}-{}", new_id());
    let mut tx = repo.pool().begin().await.expect("begin gap tx");
    let card_id = create_card_in_tx(&repo, &mut tx, label, "codex").await;
    let mut active_init = projectable_runtime_init(
        &card_id,
        label,
        "active",
        WorkerSessionState::Running,
        30_000,
    );
    active_init.kind = WorkerSessionKind::SharedSpec;
    let active = session_start_runtime_tx(&mut tx, active_init)
        .await
        .expect("start active shared-spec runtime");
    session_prepare_deferred_spec_tx(
        &mut tx,
        &deferred_projectable_placeholder_init(&card_id, &placeholder_id, 40_000),
    )
    .await
    .expect("prepare deferred projectable placeholder");
    tx.commit().await.expect("commit gap tx");

    let flipped = runtime_get_projectable_for_card_from_pool(repo.pool(), &card_id)
        .await
        .expect("worker-session projectable read")
        .expect("placeholder projectable read");
    assert_eq!(flipped.id, placeholder_id);

    let batch =
        runtime_get_projectable_for_cards_from_pool(repo.pool(), std::slice::from_ref(&card_id))
            .await
            .expect("worker-session batch projectable read");
    assert_eq!(
        batch
            .get(&card_id)
            .expect("placeholder batch projectable read")
            .id,
        placeholder_id
    );

    assert_eq!(active.status, WorkerSessionState::Running);
}

#[tokio::test]
async fn terminals_orphaned_protects_terminal_when_old_session_active_and_placeholder_present() {
    let repo = fresh_repo().await;
    let label = "terminals-orphaned-placeholder";
    let (card_id, terminal_id, initial_runtime_id) = seed_codex_terminal_card(&repo, label).await;
    let old_runtime_id = format!("rt-read-flip-{label}-old");
    let placeholder_id = format!("rt-read-flip-{label}-placeholder-{}", new_id());

    let mut tx = repo.pool().begin().await.expect("begin #744 seed tx");
    session_complete_tx(&mut tx, &initial_runtime_id, WorkerSessionState::Exited)
        .await
        .expect("complete initial codex runtime");
    let old_runtime = session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: old_runtime_id,
            card_id: card_id.clone(),
            kind: WorkerSessionKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Running,
            terminal_run_id: None,
            thread_id: Some(format!("thread-{label}-old")),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: 10_000,
        },
    )
    .await
    .expect("start old active shared-spec runtime");
    session_prepare_deferred_spec_tx(
        &mut tx,
        &deferred_projectable_placeholder_init(&card_id, &placeholder_id, 20_000),
    )
    .await
    .expect("prepare deferred shared-spec placeholder");
    tx.commit().await.expect("commit #744 seed tx");
    age_terminal_past_grace(&repo, &terminal_id).await;

    let card_session_id: Option<String> =
        sqlx::query_scalar("SELECT session_id FROM cards WHERE id = ?1")
            .bind(&card_id)
            .fetch_one(repo.pool())
            .await
            .expect("read card session pointer");
    assert_eq!(old_runtime.status, WorkerSessionState::Running);
    assert_eq!(card_session_id.as_deref(), Some(placeholder_id.as_str()));

    let orphans = repo
        .terminals_orphaned(60)
        .await
        .expect("scan orphaned terminals");
    assert!(
        !orphans.iter().any(|terminal| terminal.id == terminal_id),
        "old active worker_session.card_id must protect terminal, got: {orphans:?}"
    );
}

#[tokio::test]
async fn terminals_orphaned_reaps_terminal_when_card_has_no_active_session() {
    let repo = fresh_repo().await;
    let label = "terminals-orphaned-no-active";
    let (card_id, terminal_id, initial_runtime_id) = seed_codex_terminal_card(&repo, label).await;

    let mut tx = repo.pool().begin().await.expect("begin no-active seed tx");
    session_complete_tx(&mut tx, &initial_runtime_id, WorkerSessionState::Exited)
        .await
        .expect("complete initial codex runtime");
    tx.commit().await.expect("commit no-active seed tx");
    age_terminal_past_grace(&repo, &terminal_id).await;

    let active_session_count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM worker_sessions
               WHERE card_id = ?1
                 AND state IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(&card_id)
    .fetch_one(repo.pool())
    .await
    .expect("count active worker sessions");
    assert_eq!(active_session_count, 0);

    let orphans = repo
        .terminals_orphaned(60)
        .await
        .expect("scan orphaned terminals");
    assert!(
        orphans.iter().any(|terminal| terminal.id == terminal_id),
        "terminal without active worker_session.card_id should be orphaned, got: {orphans:?}"
    );
}

#[tokio::test]
async fn runtime_get_projectable_for_cards_from_pool_picks_pointer_history_winners() {
    let repo = fresh_repo().await;
    let active_history = seed_projectable_history(&repo, "projectable-batch-active", true).await;
    let no_active_history =
        seed_projectable_history(&repo, "projectable-batch-no-active", false).await;
    let (placeholder_card_id, placeholder_id) =
        seed_deferred_projectable_placeholder(&repo, "projectable-batch-placeholder").await;
    let active = active_history.active.as_ref().expect("active runtime");

    let card_ids = vec![
        active_history.card_id.clone(),
        no_active_history.card_id.clone(),
        placeholder_card_id.clone(),
    ];
    let actual = runtime_get_projectable_for_cards_from_pool(repo.pool(), &card_ids)
        .await
        .expect("worker-session batch projectable read");

    assert_eq!(actual.len(), 3);
    assert_eq!(
        actual
            .get(&active_history.card_id)
            .expect("active card batch runtime")
            .id,
        active.id
    );
    assert_eq!(
        actual
            .get(&no_active_history.card_id)
            .expect("no-active card batch runtime")
            .id,
        no_active_history.exited.id
    );
    assert_eq!(
        actual
            .get(&placeholder_card_id)
            .expect("placeholder card batch runtime")
            .id,
        placeholder_id
    );
    assert!(
        actual
            .values()
            .all(|runtime| runtime.id != active_history.superseded.id
                && runtime.id != no_active_history.superseded.id
                && runtime.status != WorkerSessionState::Superseded)
    );
}

#[tokio::test]
async fn worker_session_backed_reads_return_deferred_spec_placeholder() {
    let repo = fresh_repo().await;
    let placeholder_id = format!("rt-read-flip-placeholder-{}", new_id());
    let mut tx = repo.pool().begin().await.expect("begin placeholder tx");
    let card_id = create_card_in_tx(&repo, &mut tx, "placeholder", "codex").await;
    session_prepare_deferred_spec_tx(
        &mut tx,
        &WorkerSessionInit {
            id: placeholder_id.clone(),
            card_id: card_id.clone(),
            kind: WorkerSessionKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Starting,
            terminal_run_id: None,
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: 5_000,
        },
    )
    .await
    .expect("prepare deferred placeholder");
    tx.commit().await.expect("commit placeholder tx");

    let by_id = runtime_get_by_id_from_pool(repo.pool(), &placeholder_id)
        .await
        .expect("by-id read")
        .expect("placeholder by-id read");
    assert_eq!(by_id.id, placeholder_id);

    let active_for_card = runtime_get_active_for_card_from_pool(repo.pool(), &card_id)
        .await
        .expect("active-for-card read")
        .expect("placeholder active-for-card read");
    assert_eq!(active_for_card.id, placeholder_id);

    let active_for_kind =
        runtimes_active_for_kind_from_pool(repo.pool(), WorkerSessionKind::SharedSpec)
            .await
            .expect("active-for-kind read");
    assert_eq!(active_for_kind.len(), 1);
    assert_eq!(active_for_kind[0].id, placeholder_id);

    let mut tx = repo.pool().begin().await.expect("begin status tx");
    let status = runtime_current_status_tx(&mut tx, &placeholder_id)
        .await
        .expect("placeholder status read");
    tx.commit().await.expect("commit status tx");
    assert_eq!(status, WorkerSessionState::Starting);
}

#[tokio::test]
async fn deferred_spec_placeholder_rejects_non_null_session_id() {
    let repo = fresh_repo().await;
    let placeholder_id = format!("rt-placeholder-session-key-{}", new_id());
    let mut tx = repo.pool().begin().await.expect("begin placeholder tx");
    let card_id = create_card_in_tx(&repo, &mut tx, "placeholder-session-key", "codex").await;
    let mut init = deferred_projectable_placeholder_init(&card_id, &placeholder_id, 5_000);
    init.session_id = Some("future-placeholder-session".to_string());

    let err = session_prepare_deferred_spec_tx(&mut tx, &init)
        .await
        .expect_err("non-null session_id must be rejected");
    tx.commit().await.expect("commit placeholder tx");

    match err {
        WorkerSessionProjectionRepoError::Message { message } => assert_eq!(
            message,
            "deferred spec session placeholders must not have a thread, terminal run, or session"
        ),
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn cohort_b_reads_exclude_deferred_spec_placeholder_by_null_keys() {
    let repo = fresh_repo().await;
    let placeholder_id = format!("rt-cohort-b-placeholder-{}", new_id());
    let mut tx = repo.pool().begin().await.expect("begin placeholder tx");
    let card_id = create_card_in_tx(&repo, &mut tx, "cohort-b-placeholder", "codex").await;
    session_prepare_deferred_spec_tx(
        &mut tx,
        &WorkerSessionInit {
            id: placeholder_id.clone(),
            card_id,
            kind: WorkerSessionKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Starting,
            terminal_run_id: None,
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: 5_000,
        },
    )
    .await
    .expect("prepare deferred placeholder");
    tx.commit().await.expect("commit placeholder tx");

    let session_keys: (Option<String>, Option<String>, Option<String>) = sqlx::query_as(
        r#"SELECT thread_id, agent_session_id, terminal_run_id
               FROM worker_sessions
               WHERE id = ?1"#,
    )
    .bind(&placeholder_id)
    .fetch_one(repo.pool())
    .await
    .expect("placeholder worker session");
    assert_eq!(session_keys, (None, None, None));

    let by_thread = runtime_get_active_by_thread_from_pool(
        repo.pool(),
        AgentProvider::Codex,
        "missing-placeholder-thread",
    )
    .await
    .expect("by-thread read");
    assert_eq!(by_thread, None);

    let by_session = runtime_get_active_by_session_from_pool(
        repo.pool(),
        AgentProvider::Claude,
        "missing-placeholder-session",
    )
    .await
    .expect("by-session read");
    assert_eq!(by_session, None);

    let attribution = runtime_active_shared_thread_attribution_from_pool(repo.pool())
        .await
        .expect("attribution read");
    assert_eq!(attribution, Vec::<(String, String)>::new());

    let mut tx = repo
        .pool()
        .begin()
        .await
        .expect("begin terminal placeholder tx");
    let by_terminal =
        session_projection_active_for_terminal_tx(&mut tx, "missing-placeholder-terminal")
            .await
            .expect("terminal read");
    tx.commit().await.expect("commit terminal placeholder tx");
    assert_eq!(by_terminal, None);
}
