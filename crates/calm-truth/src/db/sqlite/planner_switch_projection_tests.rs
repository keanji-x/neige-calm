use super::runtime_read_flip_support::{create_card_in_tx, fresh_repo};
use super::*;
use crate::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionProjectionRepo,
};
use calm_types::worker::WorkerSessionState;
use serde_json::json;

#[tokio::test]
async fn planner_switch_session_projection_requires_role_contract_binding() {
    let repo = fresh_repo().await;
    let mut tx = repo.pool().begin().await.unwrap();
    let card = create_card_in_tx(
        &repo,
        &mut tx,
        "planner-switch",
        "codex",
        json!({"schemaVersion":1,"planner_provider":"codex"}),
    )
    .await;
    sqlx::query("UPDATE cards SET role='planner' WHERE id=?1")
        .bind(&card)
        .execute(&mut *tx)
        .await
        .unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit::shared_planner(
            "switch-projection".into(),
            card.clone(),
            AgentProvider::Codex,
            WorkerSessionState::Idle,
            Some("projection-thread".into()),
            json!({"mode":"harness"}),
            1000,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        repo.session_projection_recover_harnesses_on_boot()
            .await
            .unwrap()
            .len(),
        1
    );
    for mutation in [
        "UPDATE cards SET role='assistant'",
        "UPDATE cards SET role='planner',payload=json_set(payload,'$.planner_provider','claude')",
        "UPDATE cards SET payload=json_remove(payload,'$.planner_provider')",
    ] {
        sqlx::query(mutation).execute(repo.pool()).await.unwrap();
        assert!(
            repo.session_projection_recover_harnesses_on_boot()
                .await
                .unwrap()
                .is_empty(),
            "{mutation}"
        );
    }
    sqlx::query("UPDATE cards SET payload=json_set(payload,'$.planner_provider','codex')")
        .execute(repo.pool())
        .await
        .unwrap();
    assert_eq!(
        repo.session_projection_recover_harnesses_on_boot()
            .await
            .unwrap()
            .len(),
        1
    );
}
