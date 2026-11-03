//! Appending after the frozen read never enlarges that selection.
use super::fixture::*;
use super::*;

#[tokio::test]
async fn canonical_batch_appended_during_read_waits_for_next_turn() {
    let fx = Fixture::new().await;
    let second = content_tests::accept(&fx, "second selected before read").await;
    selection_authority_tests::configure_default_effort(
        &fx.original.repo,
        fx.harness.inner.card_id.as_str(),
    )
    .await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    observe(&fx).await;
    let config = request(&mut peer).await;
    assert_eq!(config["method"], "config/read");
    let appended = content_tests::accept(&fx, "appended for the next turn").await;
    // Enqueue the production source command while the running loop is inside
    // its real metadata read. It is processed after the original dispatch.
    let observed = fx
        .harness
        .observe_canonical_source(appended.clone(), fx.source.clone());
    tokio::pin!(observed);
    assert!(futures_util::poll!(observed.as_mut()).is_pending());
    selection_authority_tests::config_reply(&mut peer, &config).await;
    let page = request(&mut peer).await;
    assert_eq!(page["method"], "model/list");
    send(
        &mut peer,
        selection_authority_tests::catalog_reply(&page, None),
    )
    .await;
    let first = request(&mut peer).await;
    assert_eq!(first["method"], "turn/start");
    let crossing = fx.frame().await;
    assert_eq!(
        stored_plan(&fx, &crossing).await.input_refs().unwrap(),
        vec![
            AcceptedInputRef::Human {
                message: fx.message.clone()
            },
            AcceptedInputRef::Human { message: second },
        ]
    );
    assert_eq!(
        crossing
            .canonical_inputs
            .pending
            .iter()
            .map(PendingInput::identity)
            .collect::<Vec<_>>(),
        vec![appended.into()]
    );
    let text = first["params"]["input"][0]["text"].as_str().unwrap();
    assert!(!text.contains("appended for the next turn"));
    acknowledge(&fx, &mut peer, &first, "frozen-first-batch").await;
    observed.await.unwrap();
    let thread = first["params"]["threadId"].as_str().unwrap();
    notify_thread(
        &fx.daemon,
        &mut peer,
        thread,
        "turn/completed",
        json!({"id":"frozen-first-batch","status":"completed"}),
    )
    .await;
    let next = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let call = request(&mut peer).await;
            match call["method"].as_str().unwrap() {
                "turn/start" => break call,
                "config/read" => selection_authority_tests::config_reply(&mut peer, &call).await,
                "model/list" => {
                    send(
                        &mut peer,
                        selection_authority_tests::catalog_reply(&call, None),
                    )
                    .await
                }
                method => panic!("unexpected next-turn method: {method}"),
            }
        }
    })
    .await
    .unwrap();
    let next_text = next["params"]["input"][0]["text"].as_str().unwrap();
    assert_eq!(next_text.matches("appended for the next turn").count(), 1);
    assert!(!next_text.contains("canonical scheduler human body"));
    assert!(!next_text.contains("second selected before read"));
    acknowledge(&fx, &mut peer, &next, "appended-next-turn").await;
    content_tests::complete(&fx, &mut peer, thread, "appended-next-turn").await;
    let done = fx.frame().await;
    assert!(done.canonical_inputs.pending.is_empty());
    assert_eq!(done.canonical_inputs.attempts.len(), 2);
    observe(&fx).await;
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    compact_tests::shutdown(&fx.harness, &mut peer).await;
}
