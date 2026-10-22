//! Lost live notification and duplicate catch-up must reach the actual provider once.
use super::*;
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use calm_truth::db::RepoEventWrite as _;

#[tokio::test]
async fn forge_failure_result_reconciles_without_broadcast_and_is_accepted_once() {
    let mut flow = Flow::running().await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let text = "Forge operation test-merge failed: probe verdict unknown; inspect its recorded outcome before retrying.";
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
            Event::TrackWakeRequested {
                track_id: flow.track.clone().into(),
                source: "forge-action".into(),
                key: "test-merge".into(),
                text: text.into(),
            },
        )
        .await
        .unwrap();
    for _ in 0..3 {
        flow.state.dispatcher.reconcile_tick_for_test().await;
    }
    let snapshot = flow.fixture.carrier(&flow.track).await.2;
    assert_eq!(snapshot.canonical_inputs.pending.len(), 1);
    assert_eq!(snapshot.canonical_inputs.pending[0].envelope_id(), Some(id));
    assert!(matches!(
        snapshot.canonical_inputs.pending[0],
        PendingInput::System { .. }
    ));
    flow.end(TURN).await;
    let call = flow
        .next()
        .await
        .expect("persisted failure must wake the original Planner without a user message");
    let input = call["params"]["input"].to_string();
    assert_eq!(input.matches(text).count(), 1, "{input}");
    flow.accept(&call, "failure-result-turn").await;
    flow.end("failure-result-turn").await;
    flow.fixture
        .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
        .await;
    flow.state.dispatcher.reconcile_tick_for_test().await;
    assert!(
        flow.fixture
            .carrier(&flow.track)
            .await
            .2
            .canonical_inputs
            .pending
            .is_empty()
    );
    flow.close().await;
}

