//! #2600: `operation_on_track` is the scope predicate of `neige_op_show`: it yields an operation
//! only for the asked kind whose stored target is the asked Track, and never decodes another row.
use super::*;
use crate::operation::forge_action_adapter::FORGE_ACTION_KIND;

async fn insert(repo: &SqlxOperationRepo, kind: &str, payload: Value) -> OperationId {
    repo.insert_operation(
        kind,
        OperationKey {
            operation_key: new_id(),
            idempotency_key: None,
            payload_hash: "hash".into(),
        },
        payload,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn operation_on_track_matches_only_its_kind_and_track() {
    let truth = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let repo = Arc::new(SqlxOperationRepo::new(truth.pool().clone()));
    let forge = insert(&repo, FORGE_ACTION_KIND, json!({"track_id": "track-a"})).await;
    let other_kind = insert(&repo, "terminal-create", json!({"track_id": "track-a"})).await;
    let other_target = insert(&repo, FORGE_ACTION_KIND, json!({"track_id": "track-a"})).await;
    let other_track = insert(&repo, FORGE_ACTION_KIND, json!({"track_id": "track-b"})).await;
    sqlx::query("UPDATE operations SET target_type = 'card' WHERE id = ?1")
        .bind(&other_target)
        .execute(truth.pool())
        .await
        .unwrap();
    // Out-of-scope rows that no longer decode: the scoped lookup never reads them.
    for op_id in [&other_kind, &other_target, &other_track] {
        sqlx::query("UPDATE operations SET tx_output_json = '\"undecodable\"' WHERE id = ?1")
            .bind(op_id)
            .execute(truth.pool())
            .await
            .unwrap();
        assert!(
            repo.get_operation(op_id).await.is_err(),
            "{op_id} is undecodable"
        );
    }
    let runtime = tests::test_runtime(truth, repo.clone(), Vec::new());

    let found = runtime
        .operation_on_track(&forge, FORGE_ACTION_KIND, "track-a")
        .await
        .unwrap()
        .expect("the forge operation of track-a");
    assert_eq!(found.id, forge);
    for (op_id, kind, track) in [
        (forge.as_str(), FORGE_ACTION_KIND, "track-b"),
        (forge.as_str(), "terminal-create", "track-a"),
        (other_kind.as_str(), FORGE_ACTION_KIND, "track-a"),
        (other_target.as_str(), FORGE_ACTION_KIND, "track-a"),
        (other_track.as_str(), FORGE_ACTION_KIND, "track-a"),
        ("no-such-op", FORGE_ACTION_KIND, "track-a"),
    ] {
        assert!(
            runtime
                .operation_on_track(op_id, kind, track)
                .await
                .unwrap()
                .is_none(),
            "{op_id} as {kind} on {track} must not be found"
        );
    }
}
