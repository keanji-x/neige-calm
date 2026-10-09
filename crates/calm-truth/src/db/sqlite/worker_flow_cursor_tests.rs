use calm_exec::flow::{CaptureCheckpoint, CaptureOutcome, CapturePosition};

use super::{SqlxRepo, area_create_tx, card_create_with_id_tx, card_delete_tx, track_create_tx};
use crate::db::worker_flow_capture::WorkerFlowCapture;
use crate::db::{RepoOutOfDomain, RepoRead};
use crate::model::{CardRole, NewArea, NewCard, NewTrack, RequestTheme};

async fn seed_card(repo: &SqlxRepo) -> String {
    seed_worker_cards(repo, &["card-cursor"]).await.remove(0)
}

/// Worker cards with these ids on one fresh track.
pub(super) async fn seed_worker_cards(repo: &SqlxRepo, ids: &[&str]) -> Vec<String> {
    let mut tx = repo.pool().begin().await.unwrap();
    let area = area_create_tx(
        &mut tx,
        NewArea {
            name: "c".into(),
            color: "#fff".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let track = track_create_tx(
        &mut tx,
        NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "w".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        &crate::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .unwrap();
    let mut cards = Vec::new();
    for id in ids {
        let card = card_create_with_id_tx(
            &mut tx,
            (*id).into(),
            NewCard {
                track_id: track.id.clone(),
                title: None,
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
        cards.push(card.id.to_string());
    }
    tx.commit().await.unwrap();
    cards
}

/// Commit an item-less record through the production capture write; returns the stored checkpoint.
async fn commit_checkpoint(
    repo: &SqlxRepo,
    card_id: &str,
    expected: CaptureCheckpoint,
    next: CapturePosition,
) -> CaptureCheckpoint {
    let outcome = repo
        .worker_flow_capture_commit(&WorkerFlowCapture {
            card_id: card_id.into(),
            source_kind: "codex_rollout".into(),
            session_id: "unused-without-items".into(),
            track_id: None,
            expected,
            next,
            items: Vec::new(),
        })
        .await
        .unwrap();
    match outcome {
        CaptureOutcome::Applied(checkpoint) => checkpoint,
        CaptureOutcome::Stale => panic!("checkpoint compare must match"),
    }
}

fn position(
    path: &str,
    record_index: i64,
    uuid: Option<&str>,
    hash: Option<&str>,
) -> CapturePosition {
    CapturePosition {
        source_path: path.into(),
        record_index,
        byte_offset: 0,
        last_source_uuid: uuid.map(str::to_owned),
        last_line_hash: hash.map(str::to_owned),
    }
}

#[tokio::test]
async fn cursor_upsert_overwrites_allows_reset_and_cascades() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let card_id = seed_card(&repo).await;

    commit_checkpoint(
        &repo,
        &card_id,
        CaptureCheckpoint::Missing,
        position("/tmp/rollout-a.jsonl", 10, Some("uuid-a"), Some("hash-a")),
    )
    .await;
    let first = repo
        .worker_flow_cursor_get(&card_id, "codex_rollout")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.record_index, 10);
    assert_eq!(first.last_source_uuid.as_deref(), Some("uuid-a"));
    assert_eq!(first.last_line_hash.as_deref(), Some("hash-a"));

    // Age the row directly, not through the upsert under test, so the next commit's overwrite of
    // `updated_at_ms` is observable even when both commits land in the same millisecond.
    sqlx::query("UPDATE worker_flow_cursors SET updated_at_ms = 1 WHERE card_id = ?1")
        .bind(&card_id)
        .execute(repo.pool())
        .await
        .unwrap();
    let aged = CaptureCheckpoint::from(
        &repo
            .worker_flow_cursor_get(&card_id, "codex_rollout")
            .await
            .unwrap()
            .unwrap(),
    );

    let stored = commit_checkpoint(
        &repo,
        &card_id,
        aged,
        position("/tmp/rollout-b.jsonl", 3, None, None),
    )
    .await;
    let reset = repo
        .worker_flow_cursor_get(&card_id, "codex_rollout")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reset.source_path, "/tmp/rollout-b.jsonl");
    assert_eq!(reset.record_index, 3);
    assert!(reset.last_source_uuid.is_none());
    assert!(reset.last_line_hash.is_none());
    assert!(
        reset.updated_at_ms > 1,
        "the commit overwrites updated_at_ms"
    );
    assert_eq!(CaptureCheckpoint::from(&reset), stored);

    commit_checkpoint(
        &repo,
        &card_id,
        stored,
        position("/tmp/rollout-b.jsonl", 14, Some("uuid-b"), Some("hash-b")),
    )
    .await;
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
