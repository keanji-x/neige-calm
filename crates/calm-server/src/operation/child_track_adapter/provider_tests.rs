//! #1791: a child track's Planner inherits its parent Planner's `planner_provider`.

use super::tests::{
    create_child_from_task, operation, payload, seed_parent, seed_task, test_workspace_root,
};
use super::*;
use crate::db::sqlite::SqlxRepo;

/// Every production track has a Planner card, and a child inherits its `planner_provider`.
pub(super) async fn ensure_planner_card(repo: &SqlxRepo, track_id: &str) {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM cards WHERE track_id=?1 AND role='planner')",
    )
    .bind(track_id)
    .fetch_one(repo.pool())
    .await
    .unwrap();
    if exists {
        return;
    }
    let mut tx = repo.pool().begin().await.unwrap();
    card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            title: None,
            track_id: track_id.into(),
            kind: "codex".into(),
            sort: None,
            payload: planner_harness_card_payload(
                None,
                crate::session_projection_repo::AgentProvider::Codex,
            ),
        },
        CardRole::Planner,
        false,
        repo.card_role_cache(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn planner_provider_of(repo: &SqlxRepo, track_id: &str) -> Value {
    let payload: String =
        sqlx::query_scalar("SELECT payload FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(track_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    serde_json::from_str::<Value>(&payload).unwrap()
        [crate::validation::PLANNER_PROVIDER_PAYLOAD_KEY]
        .clone()
}

#[tokio::test]
async fn a_child_planner_inherits_its_parent_planners_provider() {
    for provider in ["claude", "codex"] {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let parent = seed_parent(&repo, false).await;
        sqlx::query(
            "UPDATE cards SET payload=json_set(payload,'$.planner_provider',?1) \
             WHERE track_id=?2 AND role='planner'",
        )
        .bind(provider)
        .bind(&parent)
        .execute(repo.pool())
        .await
        .unwrap();
        let task = seed_task(&repo, &parent, false).await;
        let child = create_child_from_task(&repo, &parent, &task.id).await;
        assert_eq!(planner_provider_of(&repo, &child).await, provider);
    }
}

#[tokio::test]
async fn a_parent_planner_without_a_provider_refuses_the_child() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let parent = seed_parent(&repo, false).await;
    sqlx::query(
        "UPDATE cards SET payload=json_remove(payload,'$.planner_provider') \
         WHERE track_id=?1 AND role='planner'",
    )
    .bind(&parent)
    .execute(repo.pool())
    .await
    .unwrap();
    let task = seed_task(&repo, &parent, false).await;
    let input = serde_json::to_value(payload(&task)).unwrap();
    let adapter = ChildTrackAdapter::new(
        repo.card_role_cache().clone(),
        repo.track_area_cache().clone(),
        test_workspace_root(),
    );
    let mut tx = repo.pool().begin().await.unwrap();
    let error = adapter
        .prepare_tx(&mut tx, &input, &operation(input.clone()))
        .await
        .expect_err("no provider to inherit");
    assert!(
        matches!(&error, CalmError::Conflict(message) if message.contains("planner_provider")),
        "{error:?}"
    );
}
