//! Join the original native Unknown publication before a resolution baseline.
use super::*;
use crate::shared_codex_appserver::native_owner_tests::Peer;

pub(super) async fn original_unknown(
    fixture: &Fixture,
    state: &AppState,
    peer: &mut Peer,
    track: &str,
) -> (String, String, CanonicalHarnessSnapshot) {
    let origin = crate::test_seams::PausePoint::default();
    let create_wake = crate::test_seams::PausePoint::default();
    let mut original = None;
    let initial = super::super::native_attempt_recovery_tests::initial_request_before_thread_reply(
        peer,
        || async {
            let (card, worker, frame) = fixture.carrier(track).await;
            let [pending] = frame.canonical_inputs.pending.as_slice() else {
                panic!("one original accepted pending input")
            };
            assert!(matches!(
                pending,
                calm_types::conversation::PendingInput::Human { .. }
            ));
            let identity = pending.identity();
            let source = state
                .harness
                .canonical_sources(&worker, &track.into())
                .source(&identity)
                .expect("original live request source");
            source.check_current().unwrap();
            original = Some((identity, source));
            crate::test_seams::install_pause_for_test(
                "canonical-initial-report-returned",
                &card,
                origin.clone(),
            );
            crate::test_seams::install_pause_for_test(
                "canonical-human-wake-finished",
                &card,
                create_wake.clone(),
            );
        },
    )
    .await;
    send(peer, json!({"id":initial["id"],"result":{"turn":{}}})).await;
    tokio::time::timeout(Duration::from_secs(5), origin.entered.notified())
        .await
        .expect("original dispatch committed before its publication tail");
    let (_card, worker, held) = fixture.carrier(track).await;
    assert!(matches!(
        held.canonical_inputs.attempts[0].delivery,
        InputDeliveryFact::Unknown
    ));
    // The exact create invocation finished wake_retained and released its fence.
    tokio::time::timeout(Duration::from_secs(5), create_wake.entered.notified())
        .await
        .expect("the original create wake finished");
    create_wake.release.notify_one();
    let (identity, source) = original.unwrap();
    assert!(
        state
            .harness
            .canonical_sources(&worker, &track.into())
            .source(&identity)
            .is_some()
    );
    let handle = state
        .harness
        .get(&worker)
        .expect("original installed harness");
    let ack = handle.observe_canonical_source(identity, source);
    tokio::pin!(ack);
    assert!(
        futures_util::poll!(&mut ack).is_pending(),
        "source ACK must wait for the original tail"
    );
    origin.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), &mut ack)
        .await
        .expect("original tail must finish before source ACK")
        .unwrap();
    // Use the real owner ACK as a tail join, before Keep can publish a new frame.
    let settled = fixture.carrier(track).await;
    assert_eq!(
        settled.2, held,
        "publication cannot rewrite the committed Unknown checkpoint"
    );
    settled
}

#[tokio::test]
async fn original_unknown_tail_publishes_held_feedback_before_source_ack() {
    let fixture = Fixture::new().await;
    let (state, app, cookie, _auth) = fixture.application_with_auth(false).await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Original Unknown feedback","color":"#123456"}),
    )
    .await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let created = post(&app, &cookie, "/api/tracks", "create", json!({"area_id":area["id"],"title":"","planner_provider":"codex","theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY})).await;
    let track = created["id"].as_str().unwrap();
    let (card, worker, _) = original_unknown(&fixture, &state, &mut peer, track).await;
    let expected = "The original execution outcome is unconfirmed. Its input remains held; it will not be sent again automatically.";
    assert_eq!(
        state
            .harness
            .get(&worker)
            .unwrap()
            .issuance_block()
            .await
            .as_deref(),
        Some(expected)
    );
    let run = super::super::presentation_read_feedback_tests::read_run(&app, &cookie, &card).await;
    assert_eq!(run["blocked_reason"], expected);
    fixture.shutdown_process(&state).await;
}
