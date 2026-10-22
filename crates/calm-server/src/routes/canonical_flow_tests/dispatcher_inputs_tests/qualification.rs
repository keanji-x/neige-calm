//! A durable source must not acknowledge an event after a required fact read fails.
use super::*;
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId};
use calm_truth::db::RepoEventWrite as _;
use calm_truth::db::RepoRead;

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

#[tokio::test]
async fn installed_system_source_is_refused_by_input_dispatch_in_another_repository() {
    let mut flow = Flow::running().await;
    flow.report(SYSTEM_BODY).await;
    let sources = flow
        .state
        .harness
        .canonical_sources(&flow.worker, &flow.track.clone().into());
    let source = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = flow.fixture.carrier(&flow.track).await.2;
            for pending in &snapshot.canonical_inputs.pending {
                if matches!(pending, PendingInput::System { .. })
                    && let Some(source) = sources.source(&pending.identity())
                {
                    return source;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("installed Dispatcher must mint the real accepted System source");
    assert!(matches!(
        source,
        crate::conversation_inputs::InputSource::Dispatcher(_)
    ));
    let other = Fixture::new().await;
    let daemon = crate::shared_codex_appserver::SharedCodexAppServer::new_stub(other.repo.clone());
    let backend = crate::harness::backend::PlannerBackend::codex(
        daemon.clone(),
        &flow.card,
        &flow.track,
        &flow.worker,
    )
    .unwrap();
    // There are no resources in B with these identities. A resource lookup would
    // fail differently; dispatch must reject the producer's repository first.
    let result = crate::harness::input_dispatch::initial(
        other.repo.as_ref(),
        source,
        &flow.card,
        &flow.worker,
        &calm_types::conversation::InputSubmissionId::parse("11111111111111111111111111111111")
            .unwrap(),
        &backend,
        &provider::TurnModelSelection::inherit(),
        crate::planner_permission_mode::PlannerPermissionMode::Never,
    )
    .await;
    assert!(
        matches!(result, Err(crate::error::CalmError::Forbidden(ref reason))
        if reason == "event source belongs to another installed repository context")
    );
    assert_eq!(daemon.turn_start_count_for_test(), 0);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM worker_sessions")
        .fetch_one(other.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    flow.state.dispatcher.retire();
    flow.end(TURN).await;
    flow.fixture
        .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
        .await;
    flow.close().await;
}
