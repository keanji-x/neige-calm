use super::*;
use crate::db::prelude::*;
use crate::event::EventScope;
use crate::ids::ActorId;
use crate::model::NewCard;
use crate::track_vcs::transcript_writer_tests::Fixture;
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn card_visibility_preserves_scoped_announcements_and_inheritance() {
    let fx = Fixture::new().await;
    let mut cards = Vec::new();
    for name in ["inherited", "unpublished", "other-scope", "wrong-kind"] {
        cards.push(
            fx.repo
                .card_create(NewCard {
                    track_id: fx.track.id.clone(),
                    kind: "codex".into(),
                    sort: Some(1.0),
                    payload: json!({"name": name}),
                    title: Some(name.into()),
                })
                .await
                .unwrap(),
        );
    }
    for (card, scope, added) in [
        (&fx.card, fx.track.id.clone(), true), // duplicate publication must not duplicate a row
        (&cards[2], TrackId::from("another-track"), true),
        (&cards[3], fx.track.id.clone(), false),
    ] {
        let event = if added {
            Event::CardAdded(card.clone())
        } else {
            Event::CardUpdated(card.clone())
        };
        fx.repo
            .event_append_fixture(
                ActorId::Kernel,
                EventScope::Track {
                    track: scope,
                    area: fx.track.area_id.clone(),
                },
                None,
                &event,
            )
            .await
            .unwrap();
    }
    let visibility =
        CardVisibility::AnnouncedOrInherited(BTreeSet::from([cards[0].id.to_string()]));
    let selected = cards_for_track(fx.repo.pool(), &fx.track.id, &visibility)
        .await
        .unwrap();
    assert_eq!(
        selected
            .iter()
            .map(|card| card.card.id.clone())
            .collect::<Vec<_>>(),
        [fx.card.id.clone(), cards[0].id.clone()]
    );
    let mut tx = crate::db::sqlite::begin_immediate_tx(fx.repo.db())
        .await
        .unwrap();
    for (index, card) in cards.iter().enumerate() {
        assert_eq!(
            card_in_track_tx(&mut tx, &fx.track.id, card.id.as_str(), &visibility)
                .await
                .unwrap()
                .is_some(),
            index == 0
        );
    }
    tx.rollback().await.unwrap();
    assert_eq!(
        cards_for_track(fx.repo.pool(), &fx.track.id, &CardVisibility::AllRows)
            .await
            .unwrap()
            .len(),
        5
    );
}

#[tokio::test]
async fn card_visibility_history_work_does_not_multiply_by_card_count() {
    let fx = Fixture::new().await;
    let mut conn = fx.repo.db().read_pool().acquire().await.unwrap();
    // Unpublished rows force the selector to establish absence, rather than stop at an early hit.
    for index in 0..64 {
        sqlx::query(
            "INSERT INTO cards
            (id,track_id,kind,sort,payload,role,created_at,updated_at)
            VALUES(?1,?2,'codex',0,'{}','worker',0,0)",
        )
        .bind(format!("unpublished-{index}"))
        .bind(fx.track.id.as_str())
        .execute(&mut *conn)
        .await
        .unwrap();
    }
    let measure = async |conn: &mut sqlx::SqliteConnection| {
        let steps = Arc::new(AtomicUsize::new(0));
        let count = steps.clone();
        conn.lock_handle()
            .await
            .unwrap()
            .set_progress_handler(100, move || {
                count.fetch_add(1, Ordering::Relaxed);
                true
            });
        let cards = cards_for_track(&mut *conn, &fx.track.id, &CardVisibility::announced_only())
            .await
            .unwrap();
        conn.lock_handle().await.unwrap().remove_progress_handler();
        assert_eq!(cards.len(), 1);
        steps.load(Ordering::Relaxed)
    };
    let before = measure(&mut conn).await;
    for index in 0..2048 {
        fx.repo
            .event_append_fixture(
                ActorId::Kernel,
                EventScope::Track {
                    track: fx.track.id.clone(),
                    area: fx.track.area_id.clone(),
                },
                None,
                &Event::CodexHook {
                    card_id: fx.card.id.clone(),
                    kind: "hook.codex.user_prompt_submit".into(),
                    hook_idempotency_key: format!("history-noise-{index}"),
                    payload: json!({}),
                },
            )
            .await
            .unwrap();
    }
    let after = measure(&mut conn).await;
    // The added history may be read once. It must not be re-read for each unpublished card.
    assert!(
        after - before < 2048,
        "history work multiplied across cards: {before} -> {after} progress ticks"
    );
}

#[tokio::test]
async fn card_visibility_point_lookup_retains_early_exit() {
    let fx = Fixture::new().await;
    let measure = async || {
        let mut tx = crate::db::sqlite::begin_immediate_tx(fx.repo.db())
            .await
            .unwrap();
        let ticks = Arc::new(AtomicUsize::new(0));
        let count = ticks.clone();
        tx.lock_handle()
            .await
            .unwrap()
            .set_progress_handler(100, move || {
                count.fetch_add(1, Ordering::Relaxed);
                true
            });
        let found = card_in_track_tx(
            &mut tx,
            &fx.track.id,
            fx.card.id.as_str(),
            &CardVisibility::announced_only(),
        )
        .await
        .unwrap();
        tx.lock_handle().await.unwrap().remove_progress_handler();
        assert_eq!(found.unwrap().card.id, fx.card.id);
        let result = ticks.load(Ordering::Relaxed);
        tx.rollback().await.unwrap();
        result
    };
    let before = measure().await;
    for index in 0..2048 {
        let mut card = fx.card.clone();
        card.id = CardId::from(format!("other-{index}"));
        fx.repo
            .event_append_fixture(
                ActorId::Kernel,
                EventScope::Track {
                    track: fx.track.id.clone(),
                    area: fx.track.area_id.clone(),
                },
                None,
                &Event::CardAdded(card),
            )
            .await
            .unwrap();
    }
    let after = measure().await;
    assert!(
        after.saturating_sub(before) < 10,
        "one card lookup must stop at its publication, not rebuild later history: {before} -> {after}"
    );
}
