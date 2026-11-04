//! Re-enter the actual scheduler with persisted multi-input claims and reports.
use super::fixture::*;
use super::*;
use crate::shared_codex_appserver::native_owner_tests::assert_no_request;

#[tokio::test]
async fn canonical_batch_unattempted_claim_resumes_all_original_inputs_once() {
    let fx = Fixture::new().await;
    let second = content_tests::accept(&fx, "second input in recovered batch").await;
    let (plan, expected, claim) = fx
        .original
        .repo
        .db()
        .writer()
        .with_writer(|write_session| {
            let fx = &fx;
            let second = &second;
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await.unwrap();
                let mut carrier = current_input_carrier_tx(
                    &mut tx,
                    fx.harness.inner.card_id.as_str(),
                    &fx.harness.inner.worker_session_id,
                )
                .await
                .unwrap();
                let plan = human_plan_for_pending_prefix_tx(
                    &mut tx,
                    &carrier,
                    std::num::NonZeroU32::new(2).unwrap(),
                    vec![
                        SystemRenderContext::new(
                            HarnessInputPresentation::System,
                            "original recovered batch context".into(),
                        )
                        .unwrap(),
                    ],
                )
                .await
                .unwrap();
                let expected = crate::harness::input_dispatch::hydrate_for_test(
                    &mut tx,
                    fx.harness.inner.card_id.as_str(),
                    &plan,
                )
                .await
                .unwrap();
                let claim = crate::conversation_inputs::claim_initial_tx(
                    &mut tx,
                    &fx.source,
                    &mut carrier,
                    plan.clone(),
                )
                .await
                .unwrap();
                let recovered = crate::conversation_inputs::source_inputs_tx(
                    &mut tx,
                    &InputSource::from(&fx.source),
                    &carrier,
                )
                .await
                .unwrap();
                assert!(recovered.is_unattempted_initial());
                assert_eq!(
                    recovered.identities(),
                    &[fx.message.clone().into(), second.clone().into()]
                );
                tx.commit().await.unwrap();
                (plan, expected, claim)
            })
        })
        .await
        .expect("independent write operation");
    fx.harness.shutdown().await.unwrap();
    let restored = restart(&fx).await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    assert_no_request(&mut peer).await;
    restored
        .observe_canonical_source(fx.message.clone(), fx.source.clone())
        .await
        .unwrap();
    let call = tokio::time::timeout(Duration::from_secs(5), content_tests::next_start(&mut peer))
        .await
        .expect("the original complete batch claim must resume");
    let [InputItem::Text { text }] = expected.as_slice() else {
        panic!("text input")
    };
    assert_eq!(call["params"]["input"][0]["text"], *text);
    let crossing = fx.frame().await;
    assert_eq!(
        crossing
            .canonical_inputs
            .active
            .as_ref()
            .unwrap()
            .submission_ids,
        vec![claim.submission_id]
    );
    assert_eq!(stored_plan(&fx, &crossing).await, plan);
    let running = acknowledge(&fx, &mut peer, &call, "recovered-batch").await;
    content_tests::complete(
        &fx,
        &mut peer,
        call["params"]["threadId"].as_str().unwrap(),
        "recovered-batch",
    )
    .await;
    let done = fx.frame().await;
    assert!(done.canonical_inputs.active.is_none());
    assert!(done.canonical_inputs.pending.is_empty());
    assert_eq!(
        done.canonical_inputs.attempts,
        running.canonical_inputs.attempts
    );
    compact_tests::shutdown(&restored, &mut peer).await;

    let completed = restart(&fx).await;
    for message in [fx.message.clone(), second] {
        completed
            .observe_canonical_source(message, fx.source.clone())
            .await
            .unwrap();
    }
    assert_no_request(&mut peer).await;
    assert_eq!(fx.frame().await, done);
    compact_tests::shutdown(&completed, &mut peer).await;
}

#[tokio::test]
async fn canonical_batch_unknown_delivery_survives_restart_without_resend() {
    let fx = Fixture::new().await;
    let second = content_tests::accept(&fx, "second uncertain input").await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    observe(&fx).await;
    let call = content_tests::next_start(&mut peer).await;
    let crossing = fx.frame().await;
    assert_eq!(
        stored_plan(&fx, &crossing)
            .await
            .input_refs()
            .unwrap()
            .len(),
        2
    );
    send(
        &mut peer,
        json!({"id":call["id"],"error":{"code":"malformed","message":"unknown native outcome"}}),
    )
    .await;
    let held = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = fx.frame().await;
            if frame
                .canonical_inputs
                .attempts
                .first()
                .is_some_and(|a| a.delivery == InputDeliveryFact::Unknown)
            {
                break frame;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(held.canonical_inputs.pending.is_empty());
    fx.harness.shutdown().await.unwrap();
    let restored = restart(&fx).await;
    for message in [fx.message.clone(), second] {
        restored
            .observe_canonical_source(message, fx.source.clone())
            .await
            .unwrap();
    }
    assert_no_request(&mut peer).await;
    assert_eq!(fx.frame().await, held);
    assert_eq!(
        stored_plan(&fx, &held).await,
        stored_plan(&fx, &crossing).await
    );
    compact_tests::shutdown(&restored, &mut peer).await;
}

#[tokio::test]
async fn canonical_batch_non_delivery_releases_whole_prefix_ahead_of_appended_input() {
    let fx = Fixture::new().await;
    let second = content_tests::accept(&fx, "second not dispatched input").await;
    let (client, mut peer) = connect(&fx.daemon).await;
    let blocked = client.lock_outbound_sink_for_test().await;
    observe(&fx).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.pending_requests_for_test() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let crossing = fx.frame().await;
    assert_eq!(
        stored_plan(&fx, &crossing)
            .await
            .input_refs()
            .unwrap()
            .len(),
        2
    );
    let appended = content_tests::accept(&fx, "appended after crossing marker").await;
    fx.auth.sessions.remove(&fx.session).unwrap();
    drop(blocked);
    let released = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = fx.frame().await;
            if frame
                .canonical_inputs
                .attempts
                .first()
                .is_some_and(|a| matches!(a.delivery, InputDeliveryFact::NotDispatched { .. }))
            {
                break frame;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(released.canonical_inputs.active.is_none());
    assert_eq!(
        released
            .canonical_inputs
            .pending
            .iter()
            .map(PendingInput::identity)
            .collect::<Vec<_>>(),
        vec![fx.message.clone().into(), second.into(), appended.into(),]
    );
    assert_eq!(released.canonical_inputs.attempts.len(), 1);
    assert_no_request(&mut peer).await;
    fx.harness.shutdown().await.unwrap();
}
