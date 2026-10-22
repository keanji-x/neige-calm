//! Explicit child selection and permission precedence.
use super::*;

#[tokio::test]
async fn child_track_config_declared_selection_overrides_parent() {
    use crate::operation::{OperationKey, OperationOutcome};
    let repo = std::sync::Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let parent = parent_with_plan(
        &repo,
        std::path::Path::new(""),
        TrackWorkspacePlan::ManagedUnder(test_workspace_root()),
    )
    .await
    .id
    .to_string();
    sqlx::query(
        "UPDATE cards SET payload=json_set(payload, \
         '$.planner_provider','claude','$.model','parent-model','$.reasoning_effort','low') \
         WHERE track_id=?1 AND role='planner'",
    )
    .bind(&parent)
    .execute(repo.pool())
    .await
    .unwrap();
    let task = seed_task(&repo, &parent, false).await;
    let mut input = serde_json::to_value(payload(&task)).unwrap();
    input["child_track"] = json!({"model":"sonnet", "reasoning_effort":"high"});
    let runtime = child_runtime(&repo, false).await;
    let op = runtime
        .submit(
            CHILD_TRACK_KIND,
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(task.id.clone()),
                payload_hash: crate::routes::idempotency_key::stable_payload_hash(&input).unwrap(),
            },
            input,
        )
        .await
        .unwrap();
    let result = runtime.wait(&op).await.unwrap();
    let OperationOutcome::Succeeded { result } = result.outcome else {
        panic!("{result:?}")
    };
    let stored = stored_planner(&repo, result["child_track_id"].as_str().unwrap()).await;
    assert_eq!(stored["model"], "sonnet");
    assert_eq!(stored["reasoning_effort"], "high");
    assert_eq!(stored["model_ever_set"], true);
    assert_eq!(stored["reasoning_effort_ever_set"], true);
}

#[tokio::test]
async fn child_track_config_provider_change_resets_parent_selection() {
    use crate::operation::{OperationKey, OperationOutcome};
    let repo = std::sync::Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let parent = parent_with_plan(
        &repo,
        std::path::Path::new(""),
        TrackWorkspacePlan::ManagedUnder(test_workspace_root()),
    )
    .await
    .id
    .to_string();
    sqlx::query(
        "UPDATE cards SET payload=json_set(payload, \
         '$.model','parent-model','$.reasoning_effort','high', \
         '$.model_ever_set',json('true'),'$.reasoning_effort_ever_set',json('true')) \
         WHERE track_id=?1 AND role='planner'",
    )
    .bind(&parent)
    .execute(repo.pool())
    .await
    .unwrap();
    let task = seed_task(&repo, &parent, false).await;
    let mut input = serde_json::to_value(payload(&task)).unwrap();
    input["child_track"] = json!({"provider":"claude"});
    let runtime = child_runtime(&repo, false).await;
    let op = runtime
        .submit(
            CHILD_TRACK_KIND,
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(task.id.clone()),
                payload_hash: crate::routes::idempotency_key::stable_payload_hash(&input).unwrap(),
            },
            input,
        )
        .await
        .unwrap();
    let result = runtime.wait(&op).await.unwrap();
    let OperationOutcome::Succeeded { result } = result.outcome else {
        panic!("{result:?}")
    };
    let stored = stored_planner(&repo, result["child_track_id"].as_str().unwrap()).await;
    assert_eq!(stored["planner_provider"], "claude");
    assert!(stored["model"].is_null());
    assert!(stored["reasoning_effort"].is_null());
    assert_eq!(
        crate::planner_model::CardModelSelection::from_payload(&stored).unwrap(),
        crate::planner_model::CardModelSelection::default()
    );
}

#[tokio::test]
async fn child_track_config_invalid_provider_or_model_refuses_creation() {
    use crate::operation::{OperationKey, OperationOutcome};
    for (config, needle) in [
        (
            json!({"provider":"invalid-provider"}),
            "child_track.provider",
        ),
        (
            json!({"provider":"claude", "model":"unknown-model"}),
            "unknown model",
        ),
        (json!({"provider":"grok"}), "child_track.provider"),
        (
            json!({"provider":"codex", "model":"unavailable-model"}),
            "catalog unavailable",
        ),
    ] {
        let repo = std::sync::Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let parent = seed_parent(&repo, false).await;
        let task = seed_task(&repo, &parent, false).await;
        let mut input = serde_json::to_value(payload(&task)).unwrap();
        input["child_track"] = config;
        let runtime = child_runtime(&repo, false).await;
        let op = runtime
            .submit(
                CHILD_TRACK_KIND,
                OperationKey {
                    operation_key: new_id(),
                    idempotency_key: Some(task.id.clone()),
                    payload_hash: crate::routes::idempotency_key::stable_payload_hash(&input)
                        .unwrap(),
                },
                input,
            )
            .await
            .unwrap();
        let result = runtime.wait(&op).await.unwrap();
        let OperationOutcome::Failed { last_error, .. } = result.outcome else {
            panic!("{result:?}")
        };
        assert!(last_error.contains(needle), "{last_error}");
        let child: Option<String> =
            sqlx::query_scalar("SELECT child_track_id FROM tasks WHERE id=?1")
                .bind(&task.id)
                .fetch_one(repo.pool())
                .await
                .unwrap();
        assert!(child.is_none());
    }
}

#[tokio::test]
async fn child_track_config_inherited_permission_overrides_registration() {
    use crate::acp_planner::config::{AcpPlannerConfig, AcpPlannerHost};
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let parent = seed_parent(&repo, false).await;
    sqlx::query(concat!(
        "UPDATE cards SET payload=json_set(payload,'$.planner_provider','opencode',",
        "'$.permission_mode','never') WHERE track_id=?1 AND role='planner'",
    ))
    .bind(&parent)
    .execute(repo.pool())
    .await
    .unwrap();
    let task = seed_task(&repo, &parent, false).await;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("acp.json");
    std::fs::write(&path, json!({"agents":[{"provider":"opencode","command":"/unused/opencode","args":[],"env":{},"min_agent_version":"1.0.0","default_permission_mode":"full"}]}).to_string()).unwrap();
    let host = AcpPlannerHost::new(
        Some(AcpPlannerConfig::read(&path).unwrap()),
        root.path(),
        root.path().join("shim"),
        root.path().join("sock"),
    )
    .unwrap();
    let adapter = ChildTrackAdapter::new(
        std::sync::Arc::new(host),
        repo.card_role_cache().clone(),
        repo.track_area_cache().clone(),
        test_workspace_root(),
    );
    let input = serde_json::to_value(payload(&task)).unwrap();
    let mut tx = repo.pool().begin().await.unwrap();
    let output = adapter
        .prepare_tx(&mut tx, &input, &operation(input.clone()))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let child = output.data["child_track_id"].as_str().unwrap();
    let payload: String =
        sqlx::query_scalar("SELECT payload FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(child)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&payload).unwrap()["permission_mode"],
        "never"
    );
}
