use super::session_projection::{
    runtime_current_status_tx, runtime_get_active_by_session_from_pool,
    runtime_get_active_by_thread_from_pool, runtime_get_active_for_card_from_pool,
    runtime_get_by_id_from_pool, runtimes_active_for_kind_from_pool,
};
use super::*;
use crate::model::new_id;
use crate::session_projection_repo::{AgentProvider, WorkerSessionKind};

use super::runtime_read_flip_support::*;

#[tokio::test]
async fn worker_sessions_card_id_dual_write_on_runtime_start() {
    let repo = fresh_repo().await;
    let mut tx = repo.pool().begin().await.expect("begin runtime start tx");
    let card_id = create_card_in_tx(&repo, &mut tx, "card-id-start", "codex").await;
    let init = projectable_runtime_init(
        &card_id,
        "card-id-start",
        "active",
        WorkerSessionState::Running,
        10_000,
    );
    let runtime = session_start_runtime_tx(&mut tx, init.clone())
        .await
        .expect("start runtime");
    tx.commit().await.expect("commit runtime start tx");

    assert_eq!(
        worker_session_card_id(repo.pool(), &runtime.id).await,
        Some(init.card_id)
    );
}

#[tokio::test]
async fn worker_sessions_card_id_dual_write_on_deferred_spec_placeholder() {
    let repo = fresh_repo().await;
    let placeholder_id = format!("rt-card-id-placeholder-{}", new_id());
    let mut tx = repo
        .pool()
        .begin()
        .await
        .expect("begin deferred placeholder tx");
    let card_id = create_card_in_tx(&repo, &mut tx, "card-id-placeholder", "codex").await;
    let init = deferred_projectable_placeholder_init(&card_id, &placeholder_id, 10_000);
    session_prepare_deferred_spec_tx(&mut tx, &init)
        .await
        .expect("prepare deferred placeholder");
    tx.commit().await.expect("commit deferred placeholder tx");

    assert_eq!(
        worker_session_card_id(repo.pool(), &placeholder_id).await,
        Some(init.card_id)
    );
}

#[tokio::test]
async fn card_delete_removes_placeholder_worker_session() {
    let repo = fresh_repo().await;

    let label = "card-delete-placeholder";
    let placeholder_id = format!("rt-projectable-placeholder-{label}-{}", new_id());
    let mut tx = repo
        .pool()
        .begin()
        .await
        .expect("begin card delete placeholder tx");
    let card_id = create_card_in_tx(&repo, &mut tx, label, "codex").await;
    let active = session_start_runtime_tx(
        &mut tx,
        projectable_runtime_init(
            &card_id,
            label,
            "active",
            WorkerSessionState::Running,
            10_000,
        ),
    )
    .await
    .expect("start active runtime");
    let placeholder = session_prepare_deferred_spec_tx(
        &mut tx,
        &deferred_projectable_placeholder_init(&card_id, &placeholder_id, 20_000),
    )
    .await
    .expect("prepare deferred spec placeholder");
    tx.commit()
        .await
        .expect("commit card delete placeholder tx");

    let seeded_ws: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions WHERE card_id = ?1")
            .bind(&card_id)
            .fetch_one(repo.pool())
            .await
            .expect("count seeded worker sessions");
    assert_eq!(seeded_ws, 2);

    let mut tx = repo
        .pool()
        .begin()
        .await
        .expect("begin card delete placeholder delete tx");
    card_delete_tx(&mut tx, &card_id, repo.card_role_cache())
        .await
        .expect("delete card");
    tx.commit()
        .await
        .expect("commit card delete placeholder delete tx");

    let remaining_ws: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions WHERE card_id = ?1")
            .bind(&card_id)
            .fetch_one(repo.pool())
            .await
            .expect("count remaining worker sessions");
    assert_eq!(remaining_ws, 0);

    let root_refs: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM waves WHERE root_session_id IN (?1, ?2)")
            .bind(active.id.as_str())
            .bind(placeholder.id.as_str())
            .fetch_one(repo.pool())
            .await
            .expect("count wave root refs to deleted worker sessions");
    assert_eq!(root_refs, 0);
}

#[tokio::test]
async fn assert_worker_sessions_card_id_complete_flags_active_null() {
    let repo = fresh_repo().await;
    let runtime = seed_runtime(
        &repo,
        RuntimeReadCase {
            label: "card-id-assert-active-null",
            card_kind: "codex",
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Running,
        },
        10_000,
    )
    .await;

    sqlx::query("UPDATE worker_sessions SET card_id = NULL WHERE id = ?1")
        .bind(&runtime.id)
        .execute(repo.pool())
        .await
        .expect("clear active worker session card_id");
    assert!(
        assert_worker_sessions_card_id_complete(repo.pool())
            .await
            .is_err()
    );

    sqlx::query("UPDATE worker_sessions SET state = 'failed' WHERE id = ?1")
        .bind(&runtime.id)
        .execute(repo.pool())
        .await
        .expect("mark worker session terminal");
    assert!(
        assert_worker_sessions_card_id_complete(repo.pool())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn runtime_current_status_tx_matches_runtimes_backed_start_for_all_kinds() {
    let repo = fresh_repo().await;
    for (index, case) in runtime_read_cases().into_iter().enumerate() {
        let runtime = seed_runtime(&repo, case, 1_000 + index as i64).await;
        let mut tx = repo.pool().begin().await.expect("begin read tx");
        let actual = runtime_current_status_tx(&mut tx, &runtime.id)
            .await
            .expect("status from worker_sessions");
        tx.commit().await.expect("commit read tx");
        assert_eq!(actual, runtime.status, "runtime {}", runtime.id);
    }
}

#[tokio::test]
async fn runtime_get_by_id_from_pool_matches_runtimes_backed_for_all_kinds() {
    let repo = fresh_repo().await;
    for (index, case) in runtime_read_cases().into_iter().enumerate() {
        let runtime = seed_runtime(&repo, case, 2_000 + index as i64).await;
        let mut tx = repo.pool().begin().await.expect("begin reference tx");
        let expected = session_projection_by_id_tx(&mut tx, &runtime.id)
            .await
            .expect("reference by-id read")
            .expect("runtime row");
        tx.commit().await.expect("commit reference tx");

        let actual = runtime_get_by_id_from_pool(repo.pool(), &runtime.id)
            .await
            .expect("worker-session by-id read")
            .expect("runtime from worker_sessions");
        assert_ws_backed_projection(&expected, &actual);
    }
}

#[tokio::test]
async fn runtime_get_active_for_card_from_pool_matches_runtimes_backed_for_all_kinds() {
    let repo = fresh_repo().await;
    for (index, case) in runtime_read_cases().into_iter().enumerate() {
        let runtime = seed_runtime(&repo, case, 3_000 + index as i64).await;
        let mut tx = repo.pool().begin().await.expect("begin reference tx");
        let expected = session_projection_active_for_card_tx(&mut tx, &runtime.card_id)
            .await
            .expect("reference active-for-card read")
            .expect("active runtime");
        tx.commit().await.expect("commit reference tx");

        let actual = runtime_get_active_for_card_from_pool(repo.pool(), &runtime.card_id)
            .await
            .expect("worker-session active-for-card read")
            .expect("active runtime from worker_sessions");
        assert_ws_backed_projection(&expected, &actual);
    }
}

#[tokio::test]
async fn runtime_get_active_for_card_tx_matches_runtimes_backed_for_all_kinds() {
    let repo = fresh_repo().await;
    for (index, case) in runtime_read_cases().into_iter().enumerate() {
        let runtime = seed_runtime(&repo, case, 3_500 + index as i64).await;
        let mut tx = repo.pool().begin().await.expect("begin active read tx");
        let actual = session_projection_active_for_card_tx(&mut tx, &runtime.card_id)
            .await
            .expect("worker-session active-for-card tx read")
            .expect("active runtime from worker_sessions");
        tx.commit().await.expect("commit active read tx");

        assert_ws_backed_projection(&runtime, &actual);
    }
}

#[tokio::test]
async fn runtime_get_by_id_tx_returns_superseded_runtime_by_id() {
    let repo = fresh_repo().await;
    let label = "by-id-superseded";
    let mut tx = repo.pool().begin().await.expect("begin supersede tx");
    let card_id = create_card_in_tx(&repo, &mut tx, label, "codex").await;
    let older = session_start_runtime_tx(
        &mut tx,
        projectable_runtime_init(
            &card_id,
            label,
            "older",
            WorkerSessionState::Running,
            10_000,
        ),
    )
    .await
    .expect("start older runtime");
    let newer = session_supersede_and_start_tx(
        &mut tx,
        &older.id,
        projectable_runtime_init(
            &card_id,
            label,
            "newer",
            WorkerSessionState::Running,
            20_000,
        ),
    )
    .await
    .expect("supersede older runtime");
    tx.commit().await.expect("commit supersede tx");

    let card_session_id: Option<String> =
        sqlx::query_scalar("SELECT session_id FROM cards WHERE id = ?1")
            .bind(&card_id)
            .fetch_one(repo.pool())
            .await
            .expect("read card session pointer");
    assert_eq!(card_session_id.as_deref(), Some(newer.id.as_str()));

    let mut tx = repo.pool().begin().await.expect("begin by-id read tx");
    let actual = session_projection_by_id_tx(&mut tx, &older.id)
        .await
        .expect("worker-session by-id tx read")
        .expect("superseded runtime remains readable by id");
    tx.commit().await.expect("commit by-id read tx");

    let mut expected = older;
    expected.status = WorkerSessionState::Superseded;
    expected.updated_at_ms = 20_000;
    expected.completed_at_ms = Some(20_000);
    assert_ws_backed_projection(&expected, &actual);
}

#[tokio::test]
async fn runtime_get_active_for_card_tx_returns_placeholder_during_deferred_spec_gap() {
    let repo = fresh_repo().await;

    let label = "active-card-gap";
    let placeholder_id = format!("rt-projectable-placeholder-{label}-{}", new_id());
    let mut tx = repo.pool().begin().await.expect("begin deferred gap tx");
    let card_id = create_card_in_tx(&repo, &mut tx, label, "codex").await;
    let mut active_init = projectable_runtime_init(
        &card_id,
        label,
        "active",
        WorkerSessionState::Running,
        10_000,
    );
    active_init.kind = WorkerSessionKind::SharedSpec;
    let active = session_start_runtime_tx(&mut tx, active_init)
        .await
        .expect("start old active runtime");
    session_prepare_deferred_spec_tx(
        &mut tx,
        &deferred_projectable_placeholder_init(&card_id, &placeholder_id, 20_000),
    )
    .await
    .expect("prepare deferred spec placeholder");

    let card_session_id: Option<String> =
        sqlx::query_scalar("SELECT session_id FROM cards WHERE id = ?1")
            .bind(&card_id)
            .fetch_one(&mut *tx)
            .await
            .expect("read card session pointer");
    assert_eq!(card_session_id.as_deref(), Some(placeholder_id.as_str()));

    let actual = session_projection_active_for_card_tx(&mut tx, &card_id)
        .await
        .expect("worker-session active-for-card tx read")
        .expect("placeholder active runtime is visible");
    let old = session_projection_by_id_tx(&mut tx, &active.id)
        .await
        .expect("old runtime read")
        .expect("old runtime still present");
    tx.commit().await.expect("commit deferred gap tx");

    assert_eq!(actual.id, placeholder_id);
    assert_eq!(actual.status, WorkerSessionState::Starting);
    assert_eq!(old.status, WorkerSessionState::Superseded);
}

#[tokio::test]
async fn runtimes_active_for_kind_from_pool_matches_runtimes_backed_for_all_kinds() {
    let repo = fresh_repo().await;
    let mut expected = Vec::new();
    for (index, case) in runtime_read_cases().into_iter().enumerate() {
        expected.push(seed_runtime(&repo, case, 4_000 + index as i64).await);
    }

    for runtime in expected {
        let actual = runtimes_active_for_kind_from_pool(repo.pool(), runtime.kind.clone())
            .await
            .expect("worker-session active-for-kind read");
        assert_eq!(
            actual.len(),
            1,
            "kind {:?} should not collapse with other contracts/providers",
            runtime.kind
        );
        assert_ws_backed_projection(&runtime, &actual[0]);
    }
}

#[tokio::test]
async fn runtime_get_active_by_thread_from_pool_uses_thread_key_and_isolates_provider() {
    let repo = fresh_repo().await;
    let codex = seed_runtime_with_keys(
        &repo,
        KeyedRuntimeSeed {
            label: "thread-codex",
            card_kind: "codex",
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            thread_id: Some("cohort-b-thread"),
            session_id: Some("codex-agent-session"),
            now_ms: 10_000,
        },
    )
    .await;
    seed_runtime_with_keys(
        &repo,
        KeyedRuntimeSeed {
            label: "thread-claude",
            card_kind: "claude",
            kind: WorkerSessionKind::ClaudeCard,
            agent_provider: Some(AgentProvider::Claude),
            thread_id: Some("claude-real-thread"),
            session_id: Some("cohort-b-thread"),
            now_ms: 20_000,
        },
    )
    .await;

    let actual = runtime_get_active_by_thread_from_pool(
        repo.pool(),
        AgentProvider::Codex,
        "cohort-b-thread",
    )
    .await
    .expect("worker-session by-thread read");
    assert_eq!(
        actual.as_ref().map(|runtime| runtime.id.as_str()),
        Some(codex.id.as_str())
    );
    let claude_actual = runtime_get_active_by_thread_from_pool(
        repo.pool(),
        AgentProvider::Claude,
        "cohort-b-thread",
    )
    .await
    .expect("worker-session claude by-thread read");
    assert_eq!(claude_actual, None);
}

#[tokio::test]
async fn runtime_get_active_by_session_from_pool_uses_agent_session_key_and_ignores_thread_key() {
    let repo = fresh_repo().await;
    let claude = seed_runtime_with_keys(
        &repo,
        KeyedRuntimeSeed {
            label: "session-claude",
            card_kind: "claude",
            kind: WorkerSessionKind::ClaudeCard,
            agent_provider: Some(AgentProvider::Claude),
            thread_id: Some("claude-thread-not-session"),
            session_id: Some("cohort-b-claude-session"),
            now_ms: 10_000,
        },
    )
    .await;
    seed_runtime_with_keys(
        &repo,
        KeyedRuntimeSeed {
            label: "session-codex",
            card_kind: "codex",
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            thread_id: Some("cohort-b-codex-thread"),
            session_id: Some("codex-agent-session"),
            now_ms: 20_000,
        },
    )
    .await;

    let actual = runtime_get_active_by_session_from_pool(
        repo.pool(),
        AgentProvider::Claude,
        "cohort-b-claude-session",
    )
    .await
    .expect("worker-session by-session read");
    assert_eq!(
        actual.as_ref().map(|runtime| runtime.id.as_str()),
        Some(claude.id.as_str())
    );
    let codex_thread_actual = runtime_get_active_by_session_from_pool(
        repo.pool(),
        AgentProvider::Codex,
        "cohort-b-codex-thread",
    )
    .await
    .expect("worker-session codex by-session read");
    assert_eq!(codex_thread_actual, None);
}
