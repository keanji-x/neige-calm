//! The explicit successor remains its own Initial before a later Human batch.
use super::*;

#[tokio::test]
async fn canonical_batch_retry_head_keeps_its_frozen_inputs_separate() {
    let (r, mut peer) = RetryFixture::new().await;
    let mut appended = vec![];
    for body in ["first Human after retry", "second Human after retry"] {
        let message =
            r.fx.original
                .repo
                .db()
                .writer()
                .with_writer(|write_session| {
                    let r = &r;
                    Box::pin(async move {
                        let mut tx = write_session.begin_immediate().await.unwrap();
                        let mut carrier = current_input_carrier_tx(
                            &mut tx,
                            r.fx.harness.inner.card_id.as_str(),
                            &r.fx.harness.inner.worker_session_id,
                        )
                        .await
                        .unwrap();
                        let accepted = crate::conversation_inputs::accept_human_tx(
                            &mut tx,
                            &r.fx.source.current_principal().unwrap(),
                            &mut carrier,
                            HumanPayload {
                                text: body.into(),
                                bound_attachments: vec![],
                            },
                        )
                        .await
                        .unwrap();
                        let AcceptedInputRef::Human { message } = accepted.input_ref else {
                            panic!("human")
                        };
                        tx.commit().await.unwrap();
                        message
                    })
                })
                .await
                .expect("independent write operation");
        appended.push(message);
    }
    let queued = r.fx.frame().await.canonical_inputs.pending;
    assert_eq!(queued[0].identity(), r.identity);
    r.attach().await;
    let call = r.start(&mut peer).await;
    let text = call["params"]["input"][0]["text"].as_str().unwrap();
    assert!(!text.contains("Human after retry"));
    let crossing = r.fx.frame().await;
    assert_eq!(crossing.canonical_inputs.pending, queued[1..]);
    let thread = call["params"]["threadId"].as_str().unwrap();
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":"retry-before-batch"}}}),
    )
    .await;
    r.fx.wait_phase(HarnessPhaseTag::TurnRunning).await;
    for message in appended {
        r.fx.harness
            .observe_canonical_source(message, r.fx.source.clone())
            .await
            .unwrap();
    }
    notify_thread(
        &r.fx.daemon,
        &mut peer,
        thread,
        "turn/completed",
        json!({"id":"retry-before-batch","status":"completed"}),
    )
    .await;
    let next = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let call = request(&mut peer).await;
            match call["method"].as_str().unwrap() {
                "turn/start" => break call,
                "config/read" => send(&mut peer, json!({"id":call["id"],"result":{"config":{}}})).await,
                "thread/read" => send(&mut peer, json!({"id":call["id"],"result":{"thread":{"id":thread,"status":{"type":"idle"},"turns":[]}}})).await,
                method => panic!("unexpected request after settled successor: {method}"),
            }
        }
    }).await.unwrap();
    let text = next["params"]["input"][0]["text"].as_str().unwrap();
    assert!(
        text.find("first Human after retry").unwrap()
            < text.find("second Human after retry").unwrap()
    );
    assert!(!text.contains("canonical scheduler human body"));
    assert!(!text.contains("frozen retry context"));
    assert!(r.fx.frame().await.canonical_inputs.pending.is_empty());
    send(
        &mut peer,
        json!({"id":next["id"],"error":{"code":"malformed","message":"hold next batch"}}),
    )
    .await;
    peer.close(None).await.unwrap();
    r.fx.harness.shutdown().await.unwrap();
}
