//! #1944 renamed the runs index field `idempotency_key` to `attempt_id`. A head stored before the
//! rename must not lose its untouched runs on the next incremental commit.
use super::*;

async fn head_runs_index(repo: &SqlxRepo, track_id: &TrackId) -> serde_json::Value {
    let manifest = head_manifest(repo, track_id).await;
    let entry = manifest
        .entries
        .get("runs/index.json")
        .expect("runs index entry");
    serde_json::from_str(&blob_text(repo, &entry.blob_hash).await).expect("runs index json")
}

#[tokio::test]
async fn next_run_commit_after_a_pre_rename_runs_index_reprojects_every_run() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _areas, write) = write_context();
    let mut workers = Vec::new();
    for key in ["run-a", "run-b"] {
        let worker = add_card_with_event(
            &repo,
            &bus,
            &roles,
            &write,
            &track.id,
            &area.id,
            "terminal",
            CardRole::Worker,
            json!({"schemaVersion": 1, "idempotency_key": key}),
        )
        .await;
        workers.push(worker);
    }
    let current = head_runs_index(&repo, &track.id).await;
    let mut legacy = current.clone();
    for run in legacy.as_array_mut().expect("runs index array") {
        let run = run.as_object_mut().expect("run entry");
        let attempt_id = run.remove("attempt_id").expect("attempt_id");
        run.insert("idempotency_key".into(), attempt_id);
    }
    seed_head_payload_blob(&repo, &track.id, "runs/index.json", legacy).await;

    // Touches run-b only; run-a must survive the pre-rename index.
    update_card_with_event(
        &repo,
        &bus,
        &write,
        &workers[1],
        &area.id,
        CardPatch {
            title: None,
            kind: None,
            sort: Some(1.0),
            payload: None,
            deletable: None,
        },
    )
    .await;

    let index = head_runs_index(&repo, &track.id).await;
    let ids: Vec<&str> = index
        .as_array()
        .expect("runs index array")
        .iter()
        .map(|run| run["attempt_id"].as_str().expect("attempt_id"))
        .collect();
    assert_eq!(ids, ["run-a", "run-b"], "{index}");
    assert!(!index.to_string().contains("idempotency_key"), "{index}");
}
