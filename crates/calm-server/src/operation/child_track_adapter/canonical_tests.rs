use super::tests::{operation, payload, seed_parent, seed_task, test_workspace_root};
use super::*;
use crate::db::sqlite::SqlxRepo;
#[tokio::test]
async fn u11_canonical_child_freezes_empty_carrier_before_bootstrap() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let parent = seed_parent(&repo, false).await;
    let task = seed_task(&repo, &parent, false).await;
    let input = serde_json::to_value(payload(&task)).unwrap();
    let adapter = ChildTrackAdapter::new(
        std::sync::Arc::new(
            crate::acp_planner::config::AcpPlannerHost::unconfigured_scratch().unwrap(),
        ),
        repo.card_role_cache().clone(),
        repo.track_area_cache().clone(),
        test_workspace_root(),
        std::path::PathBuf::from("/unused-objective-checkouts"),
        workspace_runtime::sweep::TeardownTargets::default(),
    );
    let mut tx = repo.pool().begin().await.unwrap();
    let output = adapter
        .prepare_tx(&mut tx, &input, &operation(input.clone()))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let card = output.data["planner_card_id"].as_str().unwrap();
    let (worker, raw): (Option<String>, String) =
        sqlx::query_as("SELECT session_id,payload FROM cards WHERE id=?1")
            .bind(card)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    let worker = worker.expect("child creation must allocate its original empty canonical carrier");
    assert_eq!(output.data["canonical_worker"], worker);
    let card_payload: Value = serde_json::from_str(&raw).unwrap();
    assert!(
        card_payload.get("prompt").is_none(),
        "seed must only be a System input: {raw}"
    );
    let snapshot: String =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    let snapshot: crate::harness::snapshot::CanonicalHarnessSnapshot =
        serde_json::from_str(&snapshot).unwrap();
    assert!(snapshot.canonical_inputs.pending.is_empty());
    let floor = snapshot.header.push_watermark;
    let wakes: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id,payload FROM events WHERE scope_track=?1 AND kind='track.wake_requested'",
    )
    .bind(output.data["child_track_id"].as_str().unwrap())
    .fetch_all(repo.pool())
    .await
    .unwrap();
    assert_eq!(wakes.len(), 1);
    assert!(
        floor < wakes[0].0,
        "creation floor {floor} must precede bootstrap {}",
        wakes[0].0
    );
    assert!(wakes[0].1.contains("child-bootstrap"));
    let event: Value = serde_json::from_str(&wakes[0].1).unwrap();
    assert_eq!(event["text"], output.data["seed"]);
    let messages: i64 =
        sqlx::query_scalar("SELECT count(*) FROM conversation_messages WHERE card_id=?1")
            .bind(card)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(messages, 0, "System bootstrap must not fabricate a Human");
}
