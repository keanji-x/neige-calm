//! Lost live notification and duplicate catch-up must reach the actual provider once.
use super::*;
use crate::event::Event;
use calm_truth::db::RepoEventWrite as _;

#[tokio::test]
async fn forge_failure_result_reconciles_without_broadcast_and_is_accepted_once() {
    let mut flow = Flow::running().await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let operation = broken_forge(&flow, "lost-live-failure").await;
    assert!(
        flow.state
            .operation_runtime
            .operation_result(&operation)
            .await
            .unwrap()
            .is_some()
    );
    let row = flow
        .fixture
        .repo
        .events_for_track(&flow.track, &["track.wake_requested"], None)
        .await
        .unwrap()
        .into_iter()
        .find(|row| {
            matches!(&row.event,
            Event::TrackWakeRequested { key, .. } if key == &operation)
        })
        .expect("the real Forge failure transaction must persist its wake");
    let id = row.id;
    let Event::TrackWakeRequested { text, .. } = row.event else {
        unreachable!()
    };
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
    assert_eq!(input.matches(text.as_str()).count(), 1, "{input}");
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

async fn broken_forge(flow: &Flow, key: &str) -> String {
    use crate::operation::{OperationKey, forge_action_adapter::FORGE_ACTION_KIND};
    let blocker = flow.fixture.root.path().join(key);
    std::fs::write(&blocker, "not a result directory").unwrap();
    let payload = json!({
        "track_id":flow.track,"card_id":flow.card,"subject":{"pr_number":42},
        "argv":["/bin/false"],"idem_key":key,
        "event_spec":{"event_kind":"forge.pr.merged","fields":{
            "head_sha":{"json_field":{"path":"/headRefOid"}},
            "merge_sha":{"json_field":{"path":"/mergeCommit/oid"}}
        }},
        "context":{},"probe":null,"cwd_lease":flow.fixture.root.path(),
        "result_path":blocker.join("result.json"),"deadline_ms":crate::model::now_ms()+60_000
    });
    flow.state
        .operation_runtime
        .submit(
            FORGE_ACTION_KIND,
            OperationKey {
                operation_key: crate::model::new_id(),
                idempotency_key: Some(key.into()),
                payload_hash: key.into(),
            },
            payload,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn preownership_notification_write_failure_recovers_on_periodic_reconcile() {
    let mut flow = Flow::running().await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let guard = crate::per_card_lock::lock_key(
        flow.state.operation_runtime.track_locks_for_test(),
        &flow.track,
    )
    .await;
    sqlx::query(
        "CREATE TRIGGER reject_failure_wake BEFORE INSERT ON events \
         WHEN NEW.kind='track.wake_requested' BEGIN \
         SELECT RAISE(ABORT, 'injected failure wake rejection'); END",
    )
    .execute(flow.fixture.repo.pool())
    .await
    .unwrap();
    let id = broken_forge(&flow, "retry-notification").await;
    assert!(
        flow.state
            .operation_runtime
            .operation_result(&id)
            .await
            .unwrap()
            .is_none()
    );
    let pause = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test("operation-busy-retry-failed", &id, pause.clone());
    drop(guard);
    tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
        .await
        .unwrap();
    assert!(
        flow.state
            .operation_runtime
            .operation_result(&id)
            .await
            .unwrap()
            .is_none(),
        "event rejection must roll back the terminal result"
    );
    sqlx::query("DROP TRIGGER reject_failure_wake")
        .execute(flow.fixture.repo.pool())
        .await
        .unwrap();
    // Simulate lease time elapsing. The production tick must discover and drive
    // the retained operation; no wait() or explicit operation drive is called.
    sqlx::query("UPDATE operations SET lease_until_ms=0 WHERE id=?1")
        .bind(&id)
        .execute(flow.fixture.repo.pool())
        .await
        .unwrap();
    pause.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            flow.state.dispatcher.reconcile_tick_for_test().await;
            if flow
                .state
                .operation_runtime
                .operation_result(&id)
                .await
                .unwrap()
                .is_some()
                && flow
                    .fixture
                    .carrier(&flow.track)
                    .await
                    .2
                    .canonical_inputs
                    .pending
                    .len()
                    == 1
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("periodic recovery must finish and admit its result without a new request");
    let settled = flow
        .state
        .operation_runtime
        .operation_result(&id)
        .await
        .unwrap()
        .is_some();
    let pending = flow
        .fixture
        .carrier(&flow.track)
        .await
        .2
        .canonical_inputs
        .pending
        .len();
    flow.end(TURN).await;
    let call = flow.next().await;
    if let Some(call) = &call {
        assert!(call["params"]["input"].to_string().contains(&id));
        flow.accept(call, "retried-failure-turn").await;
        flow.end("retried-failure-turn").await;
    }
    flow.fixture
        .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
        .await;
    flow.close().await;
    assert!(
        settled,
        "the normal periodic reconcile must retry the rolled-back failure without another submit"
    );
    assert_eq!(pending, 1);
    assert!(
        call.is_some(),
        "the retried result must reach the actual Planner"
    );
}
