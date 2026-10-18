//! A durable source must not acknowledge an event after a required fact read fails.
use super::*;
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId};

#[tokio::test]
async fn failed_required_task_lookup_keeps_the_real_event_prefix_unaccepted() {
    let mut flow = Flow::running().await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let before = flow.fixture.carrier(&flow.track).await.2;
    let card = CardId::from(flow.card.clone());
    let cursor_before = flow.state.dispatcher.push_cursor_for_test(&card);
    let event = Event::TaskFailed {
        idempotency_key: "qualified-task-lookup-failure".into(),
        reason: "The actual stored failure still needs its task qualification".into(),
        details: None,
        agent_message: None,
    };
    let id = flow
        .fixture
        .repo
        .log_pure_event(
            ActorId::KernelDispatcher,
            EventScope::Track {
                track: flow.track.clone().into(),
                area: flow.area.clone().into(),
            },
            None,
            &flow.state.events,
            &flow.state.card_role_cache,
            &flow.state.track_area_cache,
            event.clone(),
        )
        .await
        .unwrap();
    assert!(id > before.header.push_watermark);
    // SQL read faults cannot be expressed with a SQLite SELECT trigger. Move only
    // the required tasks relation; event, carrier and checkpoint reads stay real.
    sqlx::query("ALTER TABLE tasks RENAME TO qualification_tasks")
        .execute(flow.fixture.repo.pool())
        .await
        .unwrap();
    let lookup_failed = flow
        .fixture
        .repo
        .task_get("qualified-task-lookup-failure")
        .await
        .is_err();
    flow.state
        .dispatcher
        .catch_up_push(flow.track.clone().into(), event, id)
        .await;
    let after = flow.fixture.carrier(&flow.track).await.2;
    let cursor_after = flow.state.dispatcher.push_cursor_for_test(&card);
    // Stop the real retry owner before restoring the database. A later successful
    // qualification must not hide what this failing attempt acknowledged.
    flow.state.dispatcher.retire();
    sqlx::query("ALTER TABLE qualification_tasks RENAME TO tasks")
        .execute(flow.fixture.repo.pool())
        .await
        .unwrap();
    flow.end(TURN).await;
    flow.fixture
        .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
        .await;
    flow.close().await;
    assert!(lookup_failed, "only the required task read was unavailable");
    assert_eq!(
        after.canonical_inputs, before.canonical_inputs,
        "a qualification read error must not persist pending work"
    );
    assert_eq!(
        after.header.push_watermark, before.header.push_watermark,
        "a qualification read error must not acknowledge the durable prefix"
    );
    assert_eq!(
        cursor_after, cursor_before,
        "the live cursor must remain replayable after a required fact read error"
    );
}
