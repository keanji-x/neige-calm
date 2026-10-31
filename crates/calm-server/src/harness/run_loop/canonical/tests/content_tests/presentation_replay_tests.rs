//! Replays exercise the production content observer and persisted event stream.
use super::*;

async fn presentation_events(fx: &Fixture) -> Vec<Value> {
    sqlx::query_scalar::<_, String>(
        "SELECT payload FROM events WHERE kind='harness.presentation.changed' AND scope_card=?1 ORDER BY id",
    )
    .bind(fx.harness.inner.card_id.as_str())
    .fetch_all(fx.original.repo.pool())
    .await
    .unwrap()
    .iter()
    .map(|payload| serde_json::from_str(payload).unwrap())
    .collect()
}

#[tokio::test]
async fn presentation_replay_only_announces_changed_native_content() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let thread = start(&fx, &mut peer, fx.message.clone(), "replayed").await;
    let inner = &fx.harness.inner;
    let live = inner.live_claim.writer();
    live.turn_started("replayed");
    let initial_events = presentation_events(&fx).await.len();
    let mut announcements = inner.events.subscribe();
    let mut broadcast_count = 0;
    let mut previous_cursor = None;
    for (phase, text, expected) in [
        (ItemPhase::Started, "", 1),
        (ItemPhase::Started, "", 1),
        (ItemPhase::Completed, "answer", 2),
        (ItemPhase::Completed, "answer", 2),
        (ItemPhase::Started, "", 2),
        (ItemPhase::Completed, "updated answer", 3),
        (ItemPhase::Completed, "answer", 3),
    ] {
        content_observer::on_item(
            inner,
            &live,
            phase,
            json!({"threadId":thread,"turnId":"replayed",
                "item":{"id":"replayed-item","type":"agentMessage","text":text}}),
            vec![],
        )
        .await
        .unwrap();
        while let Ok(envelope) = announcements.try_recv() {
            if matches!(envelope.event, Event::HarnessPresentationChanged { .. }) {
                broadcast_count += 1;
            }
        }
        assert_eq!(
            broadcast_count, expected,
            "only actual changes may reach the frontend"
        );
        let events = presentation_events(&fx).await;
        assert_eq!(events.len() - initial_events, expected, "{phase:?}: {text}");
        let cursor = events.last().unwrap()["cursor"].clone();
        if let Some(previous) = previous_cursor {
            assert_eq!(
                cursor, previous,
                "one cursor must still announce real updates"
            );
        }
        previous_cursor = Some(cursor);
        if phase == ItemPhase::Completed || expected >= 2 {
            assert!(
                fx.registry
                    .live_replies()
                    .read(&inner.card_id)
                    .items
                    .is_empty()
            );
        }
    }
    complete(&fx, &mut peer, &thread, "replayed").await;
    stop(&fx, &mut peer).await;
}

#[tokio::test]
async fn presentation_replay_event_failure_rolls_back_content_for_retry() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let thread = start(&fx, &mut peer, fx.message.clone(), "retry-event").await;
    let inner = &fx.harness.inner;
    let live = inner.live_claim.writer();
    let before = presentation_events(&fx).await.len();
    sqlx::query(
        "CREATE TRIGGER refuse_presentation_event BEFORE INSERT ON events
        WHEN NEW.kind='harness.presentation.changed'
        BEGIN SELECT RAISE(ABORT,'event unavailable'); END",
    )
    .execute(fx.original.repo.db().writer_pool())
    .await
    .unwrap();
    let params = json!({"threadId":thread,"turnId":"retry-event",
        "item":{"id":"retry-item","type":"agentMessage","text":"answer"}});
    assert!(
        content_observer::on_item(inner, &live, ItemPhase::Completed, params.clone(), vec![])
            .await
            .is_err()
    );
    let rows = inner
        .repo
        .transcript_rows_of_thread(inner.card_id.as_str(), &thread)
        .await
        .unwrap();
    assert!(
        rows.iter()
            .all(|row| row.item_uuid.as_deref() != Some("retry-item")),
        "failed notification must roll back its content so a replay can retry"
    );
    sqlx::query("DROP TRIGGER refuse_presentation_event")
        .execute(fx.original.repo.db().writer_pool())
        .await
        .unwrap();
    content_observer::on_item(inner, &live, ItemPhase::Completed, params, vec![])
        .await
        .unwrap();
    assert_eq!(presentation_events(&fx).await.len() - before, 1);
    complete(&fx, &mut peer, &thread, "retry-event").await;
    stop(&fx, &mut peer).await;
}

#[tokio::test]
async fn presentation_replay_repairs_missing_reference_and_announces_once() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let thread = start(&fx, &mut peer, fx.message.clone(), "repair-reference").await;
    let inner = &fx.harness.inner;
    let live = inner.live_claim.writer();
    let params = json!({"threadId":thread,"turnId":"repair-reference",
        "item":{"id":"repair-item","type":"agentMessage","text":"answer"}});
    content_observer::on_item(inner, &live, ItemPhase::Completed, params.clone(), vec![])
        .await
        .unwrap();
    let before = presentation_events(&fx).await;
    let cursor = before.last().unwrap()["cursor"].clone();
    sqlx::query("UPDATE conversation_presentations SET native_item_row_id=NULL WHERE card_id=?1 AND source_kind='native_item'")
        .bind(inner.card_id.as_str()).execute(fx.original.repo.db().writer_pool()).await.unwrap();
    // A late start repairs the completed reference without opening a ghost live reply.
    content_observer::on_item(inner, &live, ItemPhase::Started, params.clone(), vec![])
        .await
        .unwrap();
    assert!(
        fx.registry
            .live_replies()
            .read(&inner.card_id)
            .items
            .is_empty()
    );
    let repaired = presentation_events(&fx).await;
    assert_eq!(repaired.len(), before.len() + 1);
    assert_eq!(repaired.last().unwrap()["cursor"], cursor);
    content_observer::on_item(inner, &live, ItemPhase::Completed, params, vec![])
        .await
        .unwrap();
    assert_eq!(presentation_events(&fx).await, repaired);
    let row: Option<i64> = sqlx::query_scalar("SELECT native_item_row_id FROM conversation_presentations WHERE card_id=?1 AND source_kind='native_item'")
        .bind(inner.card_id.as_str()).fetch_one(fx.original.repo.pool()).await.unwrap();
    assert!(row.is_some());
    complete(&fx, &mut peer, &thread, "repair-reference").await;
    stop(&fx, &mut peer).await;
}
