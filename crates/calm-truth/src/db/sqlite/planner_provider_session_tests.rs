//! #1791: a Planner session row persists the provider its mint input names.

use super::session_projection::{runtime_get_by_id_from_pool, runtimes_active_for_kind_from_pool};
use super::*;
use crate::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionProjectionRepoError,
};
use calm_types::worker::WorkerSessionState;
use serde_json::json;

use super::runtime_read_flip_support::{create_card_in_tx, fresh_repo};

async fn stored_identity(repo: &SqlxRepo, id: &str) -> (String, String, String) {
    sqlx::query_as("SELECT provider, mode, contract FROM worker_sessions WHERE id = ?1")
        .bind(id)
        .fetch_one(repo.pool())
        .await
        .expect("session row")
}

#[tokio::test]
async fn a_planner_mint_persists_the_provider_it_names() {
    let repo = fresh_repo().await;
    for (label, provider, stored) in [
        ("planner-claude", AgentProvider::Claude, "claude"),
        ("planner-codex", AgentProvider::Codex, "codex"),
    ] {
        let mut tx = repo.pool().begin().await.expect("begin");
        let card_id = create_card_in_tx(&repo, &mut tx, label, "codex").await;
        let id = format!("rt-{label}");
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit::shared_planner(
                id.clone(),
                card_id,
                provider.clone(),
                WorkerSessionState::Idle,
                Some(format!("thread-{label}")),
                json!({"mode": "harness"}),
                1_000,
            ),
        )
        .await
        .expect("start planner runtime");
        tx.commit().await.expect("commit");

        assert_eq!(
            stored_identity(&repo, &id).await,
            (stored.into(), "resumable".into(), "planner".into()),
            "{label}"
        );
        let projected = runtime_get_by_id_from_pool(repo.pool(), &id)
            .await
            .expect("by-id read")
            .expect("row");
        assert_eq!(projected.kind, WorkerSessionKind::SharedPlanner, "{label}");
        assert_eq!(projected.agent_provider, Some(provider), "{label}");
    }
    let mut planners: Vec<String> =
        runtimes_active_for_kind_from_pool(repo.pool(), WorkerSessionKind::SharedPlanner)
            .await
            .expect("active planners")
            .into_iter()
            .map(|runtime| runtime.id)
            .collect();
    planners.sort();
    assert_eq!(planners, ["rt-planner-claude", "rt-planner-codex"]);
}

#[tokio::test]
async fn a_deferred_claude_placeholder_is_a_claude_row() {
    let repo = fresh_repo().await;
    let mut tx = repo.pool().begin().await.expect("begin");
    let card_id = create_card_in_tx(&repo, &mut tx, "deferred-claude", "codex").await;
    session_prepare_deferred_planner_tx(
        &mut tx,
        &WorkerSessionInit::shared_planner(
            "rt-deferred-claude".into(),
            card_id,
            AgentProvider::Claude,
            WorkerSessionState::Starting,
            None,
            json!({"mode": "harness"}),
            1_000,
        ),
    )
    .await
    .expect("prepare deferred placeholder");
    tx.commit().await.expect("commit");
    assert_eq!(
        stored_identity(&repo, "rt-deferred-claude").await,
        ("claude".into(), "resumable".into(), "planner".into())
    );
}

#[tokio::test]
async fn a_planner_init_without_a_provider_writes_nothing() {
    let repo = fresh_repo().await;
    let mut tx = repo.pool().begin().await.expect("begin");
    let card_id = create_card_in_tx(&repo, &mut tx, "planner-unnamed", "codex").await;
    let mut init = WorkerSessionInit::shared_planner(
        "rt-planner-unnamed".into(),
        card_id,
        AgentProvider::Codex,
        WorkerSessionState::Idle,
        Some("thread-unnamed".into()),
        json!({"mode": "harness"}),
        1_000,
    );
    init.agent_provider = None;
    let err = session_start_runtime_tx(&mut tx, init)
        .await
        .expect_err("a planner row needs its provider");
    assert_eq!(
        err,
        WorkerSessionProjectionRepoError::Message {
            message: "planner runtime init rt-planner-unnamed names no provider".into()
        }
    );
}
