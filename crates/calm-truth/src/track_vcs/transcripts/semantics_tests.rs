use super::*;
use crate::db::prelude::*;
use crate::event::Event;
use crate::model::NewCard;
use crate::track_vcs::store::{commit_tree_at_tx, store_tree};
use crate::track_vcs::transcript_writer_tests::{Fixture, finish, start};
use crate::track_vcs::{MANIFEST_SCHEMA_VERSION, cat_at, commit_record};
use serde_json::json;

#[tokio::test]
async fn transcript_refresh_unchanged_blobs_are_not_rebound_but_commit_still_advances() {
    let fx = Fixture::new().await;
    let first = fx.refresh().await;
    let (counts, task) = start(fx.repo.db().clone(), fx.track.id.clone(), vec![]);
    let second = finish(task).await.unwrap();
    let first_record = commit_record(fx.repo.pool(), &first)
        .await
        .unwrap()
        .unwrap();
    let second_record = commit_record(fx.repo.pool(), &second)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(first_record.tree_hash, second_record.tree_hash);
    assert_eq!(second_record.parent_hash, Some(first));
    assert_eq!(
        second_record.author.as_deref(),
        Some(ActorId::Kernel.to_string().as_str())
    );
    assert_eq!(counts.binds(), 0);
    assert_eq!(counts.reads(), 1);
    assert_eq!(fx.refresh_count().await, 2);
}

#[tokio::test]
async fn transcript_refresh_repairs_missing_blob_even_when_parent_entry_hash_matches() {
    let fx = Fixture::new().await;
    let first = fx.refresh().await;
    let old = tree_at(fx.repo.pool(), &first).await.unwrap().unwrap();
    let path = format!("cards/{}/events.json", fx.card.id);
    let hash = &old.entries[&path].blob_hash;
    sqlx::query("DELETE FROM track_vcs_objects WHERE hash = ?1")
        .bind(hash)
        .execute(fx.repo.pool())
        .await
        .unwrap();
    let (counts, task) = start(fx.repo.db().clone(), fx.track.id.clone(), vec![]);
    let repaired = finish(task).await.unwrap();
    let current = tree_at(fx.repo.pool(), &repaired).await.unwrap().unwrap();
    assert_eq!(current.entries[&path].blob_hash, *hash);
    assert!(
        cat_at(fx.repo.pool(), &repaired, &path)
            .await
            .unwrap()
            .content
            .contains("retained transcript writer probe")
    );
    assert_eq!(counts.binds(), 1);
}

#[tokio::test]
async fn transcript_refresh_no_head_full_snapshot_uses_only_prepared_hooks() {
    let fx = Fixture::new().await;
    sqlx::query("DELETE FROM track_vcs_refs WHERE track_id = ?1")
        .bind(fx.track.id.as_str())
        .execute(fx.repo.pool())
        .await
        .unwrap();
    let (counts, task) = start(fx.repo.db().clone(), fx.track.id.clone(), vec![]);
    let commit = finish(task).await.unwrap();
    let record = commit_record(fx.repo.pool(), &commit)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.parent_hash, None);
    assert_eq!(
        counts.reads(),
        1,
        "full snapshot cannot reread or reinterpret live hooks"
    );
    for name in ["events.json", "conversation.md"] {
        assert!(
            fx.text(&commit, &fx.card.id, name)
                .await
                .contains("retained transcript writer probe")
        );
    }
    fx.text(&commit, &fx.card.id, ".meta.json").await;
}

#[tokio::test]
async fn transcript_refresh_legacy_full_snapshot_uses_only_prepared_hooks() {
    let fx = Fixture::new().await;
    let parent = head(fx.repo.pool(), &fx.track.id).await.unwrap().unwrap();
    let mut manifest = tree_at(fx.repo.pool(), &parent).await.unwrap().unwrap();
    let meta = manifest
        .entries
        .remove(&format!("cards/{}/.meta.json", fx.card.id))
        .unwrap();
    manifest
        .entries
        .insert(format!("cards/{}/meta.json", fx.card.id), meta);
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let tree = store_tree(&mut tx, MANIFEST_SCHEMA_VERSION, manifest.entries, now_ms())
        .await
        .unwrap();
    let legacy = commit_tree_at_tx(
        &mut tx,
        &fx.track.id,
        &tree,
        CommitTreeMeta {
            parent_hash: Some(&parent),
            author: None,
            event_id: None,
            message: "legacy fixture",
            manifest_schema_version: MANIFEST_SCHEMA_VERSION,
            created_at: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (counts, task) = start(fx.repo.db().clone(), fx.track.id.clone(), vec![]);
    let commit = finish(task).await.unwrap();
    let manifest = tree_at(fx.repo.pool(), &commit).await.unwrap().unwrap();
    assert!(!super::super::types::has_legacy_card_lens_paths(&manifest));
    assert_eq!(
        commit_record(fx.repo.pool(), &commit)
            .await
            .unwrap()
            .unwrap()
            .parent_hash,
        Some(legacy)
    );
    assert_eq!(counts.reads(), 1);
    for name in ["events.json", "conversation.md"] {
        assert!(
            fx.text(&commit, &fx.card.id, name)
                .await
                .contains("retained transcript writer probe")
        );
    }
}

#[tokio::test]
async fn transcript_refresh_keeps_unannounced_raw_card_hidden() {
    let fx = Fixture::new().await;
    let hidden = fx
        .repo
        .card_create(NewCard {
            track_id: fx.track.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
            title: None,
        })
        .await
        .unwrap();
    let (counts, task) = start(fx.repo.db().clone(), fx.track.id.clone(), vec![]);
    let commit = finish(task).await.unwrap();
    let manifest = tree_at(fx.repo.pool(), &commit).await.unwrap().unwrap();
    assert!(
        !manifest
            .entries
            .keys()
            .any(|path| path.starts_with(&format!("cards/{}/", hidden.id)))
    );
    assert_eq!(counts.reads(), 1);
}

#[tokio::test]
async fn transcript_refresh_both_providers_keep_order_and_existing_projection_bytes() {
    let fx = Fixture::new().await;
    let claude = fx.log(Event::ClaudeHook {
        card_id: fx.card.id.clone(), kind: "hook.claude.user_prompt_submit".into(),
        hook_idempotency_key: "claude projection probe".into(),
        payload: json!({"hook_event_name": "UserPromptSubmit", "prompt": "claude projection probe"}),
    }).await;
    let codex = fx.hook("codex projection probe").await;
    let commit = fx.refresh().await;
    // Compare the existing authoritative live owner rather than hand-copying either renderer.
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let events = hook_events_for_card_tx(&mut tx, &fx.track.id, &fx.card.id)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    let expected = hook_events_json(&events).unwrap();
    assert_eq!(
        fx.text(&commit, &fx.card.id, "events.json")
            .await
            .as_bytes(),
        expected.bytes
    );
    assert_eq!(
        fx.text(&commit, &fx.card.id, "conversation.md").await,
        conversation_markdown(&fx.card.id, &events)
    );
    let ids: Vec<_> = events.iter().map(|event| event.event_id).collect();
    assert_eq!(&ids[ids.len() - 2..], &[claude, codex]);
}

#[tokio::test]
async fn transcript_refresh_publication_failure_rolls_back_objects_commit_and_ref() {
    let fx = Fixture::new().await;
    let parent = head(fx.repo.pool(), &fx.track.id).await.unwrap();
    let objects: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM track_vcs_objects")
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    sqlx::query(include_str!("reject_refresh.sql"))
        .execute(fx.repo.pool())
        .await
        .unwrap();
    let (counts, task) = start(fx.repo.db().clone(), fx.track.id.clone(), vec![]);
    let error = finish(task).await.unwrap_err();
    assert!(error.to_string().contains("reject transcript refresh"));
    assert_eq!(
        counts.binds(),
        2,
        "failure follows real blob publication attempts"
    );
    assert_eq!(head(fx.repo.pool(), &fx.track.id).await.unwrap(), parent);
    assert_eq!(fx.refresh_count().await, 0);
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM track_vcs_objects")
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    assert_eq!(remaining, objects);
}

#[tokio::test]
async fn transcript_refresh_missing_prepared_input_fails_instead_of_rendering_live_hooks() {
    let fx = Fixture::new().await;
    let prepared = PreparedHooks::new();
    let mut entries = BTreeMap::new();
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let error = HookInputs::Prepared(&prepared)
        .put_card(&mut tx, &mut entries, &fx.track.id, &fx.card.id, now_ms())
        .await
        .unwrap_err();
    tx.rollback().await.unwrap();
    assert!(error.to_string().contains("missing prepared transcript"));
    assert!(entries.is_empty());
}
