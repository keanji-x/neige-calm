use super::*;
use crate::db::prelude::*;
use crate::event::Event;
use crate::model::{CardPatch, NewCard, NewTrack, RequestTheme};
use crate::track_vcs::transcript_writer_tests::{Fixture, Phase, finish, start};
use serde_json::json;

#[tokio::test]
async fn transcript_refresh_preserves_intervening_head_and_metadata_without_reprepare() {
    let fx = Fixture::new().await;
    let (pause, task) = start(
        fx.repo.pool().clone(),
        fx.track.id.clone(),
        vec![Phase::BeforeWrite],
    );
    pause.entered().await;
    let updated = fx
        .repo
        .card_update(
            fx.card.id.as_str(),
            CardPatch {
                payload: Some(json!({"schemaVersion": 1, "label": "fresh current metadata"})),
                ..CardPatch::default()
            },
        )
        .await
        .unwrap();
    fx.log(Event::CardUpdated(updated)).await;
    let parent = head(fx.repo.pool(), &fx.track.id).await.unwrap().unwrap();
    pause.release();
    let commit = finish(task).await.unwrap();
    let record = super::super::commit_record(fx.repo.pool(), &commit)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.parent_hash.as_deref(), Some(parent.as_str()));
    assert!(
        fx.text(&commit, &fx.card.id, ".payload.json")
            .await
            .contains("fresh current metadata")
    );
    assert_eq!(
        pause.attempts(),
        1,
        "a HEAD-only change cannot invalidate hook inputs"
    );
    assert_eq!(pause.reads(), 1);
}

#[tokio::test]
async fn transcript_refresh_reprepares_after_hook_append_without_stale_publication() {
    let fx = Fixture::new().await;
    let (pause, task) = start(
        fx.repo.pool().clone(),
        fx.track.id.clone(),
        vec![Phase::BeforeWrite],
    );
    pause.entered().await;
    fx.hook("appended after preparation").await;
    pause.release();
    let commit = finish(task).await.unwrap();
    for name in ["events.json", "conversation.md"] {
        assert!(
            fx.text(&commit, &fx.card.id, name)
                .await
                .contains("appended after preparation")
        );
    }
    assert_eq!(pause.attempts(), 2);
    assert_eq!(fx.refresh_count().await, 1);
    assert_eq!(pause.binds(), 2, "obsolete objects must never be inserted");
}

#[tokio::test]
async fn transcript_refresh_reuses_unchanged_card_after_hook_append() {
    let fx = Fixture::new().await;
    let unchanged = fx
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
    let bus = crate::event::EventBus::new();
    for event in [
        Event::CardAdded(unchanged.clone()),
        Event::CodexHook {
            card_id: unchanged.id.clone(),
            kind: "hook.codex.user_prompt_submit".into(),
            hook_idempotency_key: "unchanged card retained hook".into(),
            payload: json!({
                "hook_event_name": "UserPromptSubmit",
                "prompt": "unchanged card retained hook",
            }),
        },
    ] {
        fx.repo
            .log_pure_event(
                crate::ids::ActorId::Kernel,
                crate::event::EventScope::Card {
                    card: unchanged.id.clone(),
                    track: fx.track.id.clone(),
                    area: fx.track.area_id.clone(),
                },
                None,
                &bus,
                fx.repo.card_role_cache(),
                fx.repo.track_area_cache(),
                event,
            )
            .await
            .unwrap();
    }
    let (pause, task) = start(
        fx.repo.pool().clone(),
        fx.track.id.clone(),
        vec![Phase::BeforeWrite],
    );
    pause.entered().await;
    assert_eq!(pause.reads(), 2);
    fx.hook("changed card appended hook").await;
    let parent = head(fx.repo.pool(), &fx.track.id).await.unwrap();
    pause.release();
    let commit = finish(task).await.unwrap();
    for name in ["events.json", "conversation.md"] {
        assert!(
            fx.text(&commit, &fx.card.id, name)
                .await
                .contains("changed card appended hook")
        );
        assert!(
            fx.text(&commit, &unchanged.id, name)
                .await
                .contains("unchanged card retained hook")
        );
    }
    let record = super::super::commit_record(fx.repo.pool(), &commit)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.parent_hash, parent);
    assert_eq!(pause.attempts(), 2);
    assert_eq!(fx.refresh_count().await, 1);
    assert_eq!(
        pause.reads(),
        3,
        "only the changed card must be reinterpreted"
    );
}

#[tokio::test]
async fn transcript_refresh_reprepares_after_selected_hook_delete_with_same_max_and_watermark() {
    let fx = Fixture::new().await;
    let removed = fx.hook("removed middle selected hook").await;
    let maximum = fx.hook("unchanged maximum hook").await;
    sqlx::query("INSERT INTO retention_meta (key,value) VALUES ('events_prune_watermark', ?1)")
        .bind(maximum + 100)
        .execute(fx.repo.pool())
        .await
        .unwrap();
    let (pause, task) = start(
        fx.repo.pool().clone(),
        fx.track.id.clone(),
        vec![Phase::BeforeWrite],
    );
    pause.entered().await;
    sqlx::query("DELETE FROM events WHERE id = ?1")
        .bind(removed)
        .execute(fx.repo.pool())
        .await
        .unwrap();
    pause.release();
    let commit = finish(task).await.unwrap();
    for name in ["events.json", "conversation.md"] {
        let text = fx.text(&commit, &fx.card.id, name).await;
        assert!(!text.contains("removed middle selected hook"));
        assert!(text.contains("unchanged maximum hook"));
    }
    assert_eq!(pause.attempts(), 2);
    assert_eq!(fx.refresh_count().await, 1);
    assert_eq!(pause.binds(), 2);
    assert_eq!(
        fx.repo.events_prune_watermark().await.unwrap(),
        maximum + 100
    );
}

#[tokio::test]
async fn transcript_refresh_reprepares_after_card_announcement() {
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
    let (pause, task) = start(
        fx.repo.pool().clone(),
        fx.track.id.clone(),
        vec![Phase::BeforeWrite],
    );
    pause.entered().await;
    fx.log(Event::CardAdded(hidden.clone())).await;
    pause.release();
    let commit = finish(task).await.unwrap();
    assert_eq!(pause.attempts(), 2);
    for name in ["events.json", "conversation.md"] {
        fx.text(&commit, &hidden.id, name).await;
    }
    assert_eq!(fx.refresh_count().await, 1);
}

#[tokio::test]
async fn transcript_refresh_does_not_restore_deleted_card_transcripts() {
    let fx = Fixture::new().await;
    let (pause, task) = start(
        fx.repo.pool().clone(),
        fx.track.id.clone(),
        vec![Phase::BeforeWrite],
    );
    pause.entered().await;
    fx.repo.card_delete(fx.card.id.as_str()).await.unwrap();
    fx.log(Event::CardDeleted {
        id: fx.card.id.clone(),
        track_id: fx.track.id.clone(),
    })
    .await;
    pause.release();
    let commit = finish(task).await.unwrap();
    let manifest = tree_at(fx.repo.pool(), &commit).await.unwrap().unwrap();
    assert!(
        !manifest
            .entries
            .keys()
            .any(|path| path.starts_with(&format!("cards/{}/", fx.card.id)))
    );
    assert_eq!(pause.attempts(), 2);
    assert_eq!(pause.binds(), 0);
    assert_eq!(fx.refresh_count().await, 1);
}

#[tokio::test]
async fn transcript_refresh_does_not_publish_prepared_hooks_for_moved_card() {
    let fx = Fixture::new().await;
    let other = fx
        .repo
        .track_create(NewTrack {
            template_input: None,
            area_id: fx.track.area_id.clone(),
            title: "other track".into(),
            sort: None,
            cwd: "/other-transcript-track".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let (pause, task) = start(
        fx.repo.pool().clone(),
        fx.track.id.clone(),
        vec![Phase::BeforeWrite],
    );
    pause.entered().await;
    sqlx::query("UPDATE cards SET track_id = ?1 WHERE id = ?2")
        .bind(other.id.as_str())
        .bind(fx.card.id.as_str())
        .execute(fx.repo.pool())
        .await
        .unwrap();
    pause.release();
    let commit = finish(task).await.unwrap();
    // A raw move does not itself rewrite the old manifest. The refresh must preserve that
    // current parent rather than inject freshly prepared data for a card it no longer owns.
    assert!(
        !fx.text(&commit, &fx.card.id, "events.json")
            .await
            .contains("retained transcript writer probe")
    );
    assert_eq!(pause.attempts(), 2);
    assert_eq!(pause.binds(), 0);
}

#[tokio::test]
async fn transcript_refresh_fails_on_current_corrupt_card_before_any_publication() {
    let fx = Fixture::new().await;
    let parent = head(fx.repo.pool(), &fx.track.id).await.unwrap();
    let (pause, task) = start(
        fx.repo.pool().clone(),
        fx.track.id.clone(),
        vec![Phase::BeforeWrite],
    );
    pause.entered().await;
    sqlx::query("UPDATE cards SET payload = '{' WHERE id = ?1")
        .bind(fx.card.id.as_str())
        .execute(fx.repo.pool())
        .await
        .unwrap();
    pause.release();
    assert!(finish(task).await.is_err());
    assert_eq!(head(fx.repo.pool(), &fx.track.id).await.unwrap(), parent);
    assert_eq!(fx.refresh_count().await, 0);
    assert_eq!(pause.binds(), 0);
}

#[tokio::test]
async fn transcript_refresh_defers_after_bounded_selected_hook_churn() {
    let fx = Fixture::new().await;
    let (pause, task) = start(
        fx.repo.pool().clone(),
        fx.track.id.clone(),
        vec![Phase::BeforeWrite; PREPARATION_ATTEMPTS],
    );
    let mut parent = None;
    for index in 0..PREPARATION_ATTEMPTS {
        pause.entered().await;
        fx.hook(&format!("churned hook {index}")).await;
        parent = head(fx.repo.pool(), &fx.track.id).await.unwrap();
        pause.release();
    }
    assert!(finish(task).await.is_err());
    assert_eq!(head(fx.repo.pool(), &fx.track.id).await.unwrap(), parent);
    assert_eq!(pause.attempts(), PREPARATION_ATTEMPTS);
    assert_eq!(pause.binds(), 0);
    assert_eq!(fx.refresh_count().await, 0);
}
