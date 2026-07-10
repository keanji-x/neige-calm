use super::{SqlxRepo, card_create_with_id_tx, card_delete_tx, cove_create_tx, wave_create_tx};
use crate::db::{RepoOutOfDomain, RepoRead};
use crate::model::{CardRole, NewCard, NewCove, NewWave, RequestTheme};

async fn seed_card(repo: &SqlxRepo) -> String {
    let mut tx = repo.pool().begin().await.unwrap();
    let cove = cove_create_tx(
        &mut tx,
        NewCove {
            name: "c".into(),
            color: "#fff".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let wave = wave_create_tx(
        &mut tx,
        NewWave {
            cove_id: cove.id.clone(),
            title: "w".into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        repo.wave_cove_cache(),
    )
    .await
    .unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        "card-cursor".into(),
        NewCard {
            wave_id: wave.id.clone(),
            kind: "worker".into(),
            sort: None,
            payload: serde_json::json!({}),
        },
        CardRole::Worker,
        true,
        repo.card_role_cache(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    card.id.to_string()
}

#[tokio::test]
async fn cursor_upsert_overwrites_allows_reset_and_cascades() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let card_id = seed_card(&repo).await;

    repo.worker_flow_cursor_upsert(
        &card_id,
        "codex_rollout",
        "/tmp/rollout-a.jsonl",
        10,
        0,
        Some("uuid-a"),
        Some("hash-a"),
        100,
    )
    .await
    .unwrap();
    let first = repo
        .worker_flow_cursor_get(&card_id, "codex_rollout")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.record_index, 10);
    assert_eq!(first.last_source_uuid.as_deref(), Some("uuid-a"));
    assert_eq!(first.last_line_hash.as_deref(), Some("hash-a"));

    repo.worker_flow_cursor_upsert(
        &card_id,
        "codex_rollout",
        "/tmp/rollout-b.jsonl",
        3,
        0,
        None,
        None,
        200,
    )
    .await
    .unwrap();
    let reset = repo
        .worker_flow_cursor_get(&card_id, "codex_rollout")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reset.source_path, "/tmp/rollout-b.jsonl");
    assert_eq!(reset.record_index, 3);
    assert!(reset.last_source_uuid.is_none());
    assert!(reset.last_line_hash.is_none());
    assert_eq!(reset.updated_at_ms, 200);

    repo.worker_flow_cursor_upsert(
        &card_id,
        "codex_rollout",
        "/tmp/rollout-b.jsonl",
        14,
        0,
        Some("uuid-b"),
        Some("hash-b"),
        300,
    )
    .await
    .unwrap();
    assert_eq!(
        repo.worker_flow_cursor_get(&card_id, "codex_rollout")
            .await
            .unwrap()
            .unwrap()
            .record_index,
        14
    );

    let mut tx = repo.pool().begin().await.unwrap();
    card_delete_tx(&mut tx, &card_id, repo.card_role_cache())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        repo.worker_flow_cursor_get(&card_id, "codex_rollout")
            .await
            .unwrap()
            .is_none(),
        "cursor must cascade with its card"
    );
}
