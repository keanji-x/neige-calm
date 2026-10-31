//! Passive waits use real source facts, held responders and native executions.
use super::*;
use crate::harness::held_requests::{ConnectionId, HeldRequestMessage, HeldResponder, RequestKey};
use crate::test_seams::{PausePoint, install_pause_for_test};

struct Answer(oneshot::Sender<usize>);
impl HeldResponder for Answer {
    fn respond(self: Box<Self>, option: usize) {
        let _ = self.0.send(option);
    }
}

async fn pause_after_progress(fx: &Fixture) -> PausePoint {
    let card = fx.harness.inner.card_id.as_str();
    let advance = PausePoint::default();
    install_pause_for_test("canonical-before-progress-advance", card, advance.clone());
    fx.harness.request_progress();
    tokio::time::timeout(Duration::from_secs(3), advance.entered.notified())
        .await
        .unwrap();
    let finished = PausePoint::default();
    install_pause_for_test("canonical-tick-returned", card, finished.clone());
    advance.release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), finished.entered.notified())
        .await
        .unwrap();
    finished
}

#[tokio::test]
async fn held_answer_wakes_without_polling_and_preserves_the_paused_budget() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    super::control_tests::running(&fx, &mut peer).await;
    let original = fx.frame().await;
    let (answered, answer) = oneshot::channel();
    fx.harness
        .held_request_sender()
        .send(HeldRequestMessage::Open {
            request_key: RequestKey("passive-budget".into()),
            connection: ConnectionId("passive-source".into()),
            questions: vec![crate::event::AskQuestion {
                title: "Continue?".into(),
                options: vec!["Yes".into(), "No".into()],
            }],
            responder: Box::new(Answer(answered)),
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !fx.harness.watchdog_saw_held_request_for_test().await {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    fx.harness
        .rewind_turn_clock_for_test(Duration::from_secs(3600))
        .await;
    let finished = pause_after_progress(&fx).await;
    let card = fx.harness.inner.card_id.as_str();
    let probe = PausePoint::default();
    install_pause_for_test("canonical-before-end-probe", card, probe.clone());
    finished.release.notify_one();
    let polled = tokio::time::timeout(Duration::from_millis(250), probe.entered.notified())
        .await
        .is_ok();
    probe.release.notify_one();
    crate::test_seams::take("canonical-before-end-probe", card);
    assert!(
        tokio::time::timeout(Duration::from_millis(150), request(&mut peer))
            .await
            .is_err(),
        "waiting on a user must not consume the turn budget"
    );

    let asks = fx.harness.held_requests().ask_ids();
    assert_eq!(asks.len(), 1);
    let ask = *asks.iter().next().unwrap();
    let resumed = PausePoint::default();
    install_pause_for_test("canonical-before-progress-advance", card, resumed.clone());
    // Use the real effect owner and responder table, in the production answer order.
    let write = crate::state::WriteContext::new(
        fx.harness.inner.card_role_cache.clone(),
        fx.harness.inner.track_area_cache.clone(),
    );
    let track = fx.harness.inner.track_id.clone();
    crate::db::write_with_actor_events_typed::<(), _>(
        fx.original.repo.as_ref(),
        None,
        &fx.harness.inner.events,
        &write,
        move |tx| {
            Box::pin(async move {
                let (scope, events) = crate::ask::ask_answered_tx(
                    tx,
                    &track,
                    ask,
                    vec![crate::event::AskAnswer::Option(0)],
                )
                .await?;
                Ok((
                    (),
                    events
                        .into_iter()
                        .map(|event| (ActorId::User, scope.clone(), event))
                        .collect(),
                ))
            })
        },
    )
    .await
    .unwrap();
    fx.harness.held_requests().take(ask).unwrap().respond(0);
    assert_eq!(answer.await.unwrap(), 0);
    if !polled {
        tokio::time::timeout(Duration::from_secs(1), resumed.entered.notified())
            .await
            .expect("taking the answered request must wake the sleeping owner");
        resumed.release.notify_one();
    } else {
        crate::test_seams::take("canonical-before-progress-advance", card);
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        while fx.harness.watchdog_saw_held_request_for_test().await {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(150), request(&mut peer))
            .await
            .is_err(),
        "the resumed watchdog must exclude the completed user wait"
    );
    fx.harness
        .rewind_turn_clock_for_test(Duration::from_secs(49))
        .await;
    let warning = tokio::time::timeout(Duration::from_secs(2), request(&mut peer))
        .await
        .unwrap();
    assert_eq!(warning["method"], "turn/steer");
    send(
        &mut peer,
        json!({"id":warning["id"],"result":{"turnId":original.header.last_turn_id}}),
    )
    .await;
    notify_thread(
        &fx.daemon,
        &mut peer,
        original.header.last_thread_id.as_deref().unwrap(),
        "turn/completed",
        json!({"id":original.header.last_turn_id,"status":"completed"}),
    )
    .await;
    fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    peer.close(None).await.unwrap();
    fx.harness.shutdown().await.unwrap();
    assert!(
        !polled,
        "an unchanged held execution must not use fifty-ms probes"
    );
}
