use super::transcript_writer_tests::Fixture;
use super::*;
use crate::db::sqlite::{append_decision_events_in_tx, begin_immediate_tx, card_update_tx};
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use crate::model::CardPatch;
use serde_json::json;

async fn counts(fixture: &Fixture) -> (i64, i64, i64) {
    sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM events), \
                (SELECT COUNT(*) FROM track_vcs_commits), \
                (SELECT COUNT(*) FROM track_vcs_objects)",
    )
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn unprojected_event_keeps_its_receipt_without_history_writes() {
    let f = Fixture::new().await;
    let before = counts(&f).await;
    let previous = head(f.repo.pool(), &f.track.id).await.unwrap();

    f.hook("new retained input").await;

    let after = counts(&f).await;
    assert_eq!(
        after.0,
        before.0 + 1,
        "the event receipt must remain durable"
    );
    assert_eq!(head(f.repo.pool(), &f.track.id).await.unwrap(), previous);
    assert_eq!((after.1, after.2), (before.1, before.2));
    // Hook events are intentionally projected at the turn boundary, even when
    // their immediate event append has no history delta.
    let refreshed = f.refresh().await;
    assert!(
        f.text(&refreshed, &f.card.id, "conversation.md")
            .await
            .contains("new retained input")
    );
}

#[tokio::test]
async fn unprojected_event_does_not_bootstrap_a_history_tree() {
    let f = Fixture::new().await;
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    sqlx::query("DELETE FROM track_vcs_refs WHERE track_id=?1")
        .bind(f.track.id.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM track_vcs_commits WHERE track_id=?1")
        .bind(f.track.id.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let before = counts(&f).await;

    f.hook("before the first projection").await;

    let after = counts(&f).await;
    assert_eq!(after.0, before.0 + 1);
    assert_eq!(head(f.repo.pool(), &f.track.id).await.unwrap(), None);
    assert_eq!((after.1, after.2), (before.1, before.2));
    let refreshed = f.refresh().await;
    assert_eq!(
        head(f.repo.pool(), &f.track.id).await.unwrap(),
        Some(refreshed)
    );
}

#[tokio::test]
async fn mixed_event_batch_keeps_the_projected_change_and_hook() {
    let f = Fixture::new().await;
    let before = counts(&f).await;
    let previous = head(f.repo.pool(), &f.track.id).await.unwrap();
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    let updated = card_update_tx(
        &mut tx,
        f.card.id.as_str(),
        CardPatch {
            payload: Some(json!({"schemaVersion": 1, "task": "changed content"})),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let events = [
        Event::CodexHook {
            card_id: f.card.id.clone(),
            kind: "hook.codex.user_prompt_submit".into(),
            hook_idempotency_key: "mixed-hook".into(),
            payload: json!({"hook_event_name": "UserPromptSubmit", "prompt": "mixed retained input"}),
        },
        Event::CardUpdated(updated),
    ];
    let ids = append_decision_events_in_tx(
        &mut tx,
        &ActorId::Kernel,
        &EventScope::Card {
            card: f.card.id.clone(),
            track: f.track.id.clone(),
            area: f.track.area_id.clone(),
        },
        None,
        &events,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let after = counts(&f).await;
    assert_eq!(ids.len(), 2);
    assert_eq!(after.0, before.0 + 2);
    assert_eq!(after.1, before.1 + 1);
    let current = head(f.repo.pool(), &f.track.id).await.unwrap();
    assert_ne!(current, previous);
    let commit = current.unwrap();
    assert!(
        f.text(&commit, &f.card.id, ".payload.json")
            .await
            .contains("changed content")
    );
    assert!(
        f.text(&commit, &f.card.id, "conversation.md")
            .await
            .contains("mixed retained input")
    );
}
