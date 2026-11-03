//! Batches use the actual scheduler, canonical claims and native transport.
use super::*;
mod append;
mod boundaries;
mod fixture;
mod recovery;
mod selection;

#[tokio::test]
async fn canonical_batch_queued_humans_share_one_turn_in_order() {
    let fx = Fixture::new().await;
    let mut images = vec![];
    let mut messages = vec![fx.message.clone()];
    for (body, path) in [
        ("second queued human", "/tmp/canonical-batch-second.png"),
        ("third queued human", "/tmp/canonical-batch-third.png"),
    ] {
        let attachment = calm_types::planner_attachment::BoundAttachment {
            id: calm_types::planner_attachment::AttachmentId::parse(&format!(
                "{}.png",
                uuid::Uuid::new_v4()
            ))
            .unwrap(),
            size: 10,
            path: path.into(),
        };
        messages.push(
            fixture::accept_payload(
                &fx,
                HumanPayload {
                    text: body.into(),
                    bound_attachments: vec![attachment],
                },
            )
            .await,
        );
        images.push(path);
    }
    let (_client, mut peer) = connect(&fx.daemon).await;
    fx.harness
        .observe_canonical_source(fx.message.clone(), fx.source.clone())
        .await
        .unwrap();
    let call = tokio::time::timeout(Duration::from_secs(5), content_tests::next_start(&mut peer))
        .await
        .unwrap();
    let input = call["params"]["input"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    let crossing = fx.frame().await;
    let plan = fixture::stored_plan(&fx, &crossing).await;
    let running = fixture::acknowledge(&fx, &mut peer, &call, "one-human-batch").await;
    content_tests::complete(
        &fx,
        &mut peer,
        call["params"]["threadId"].as_str().unwrap(),
        "one-human-batch",
    )
    .await;
    let done = fx.frame().await;
    fixture::observe(&fx).await;
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    compact_tests::shutdown(&fx.harness, &mut peer).await;

    assert!(input.contains("second queued human"), "{input}");
    assert!(input.contains("third queued human"), "{input}");
    assert!(
        input.find("canonical scheduler human body").unwrap()
            < input.find("second queued human").unwrap()
    );
    assert!(input.find("second queued human").unwrap() < input.find("third queued human").unwrap());
    for body in [
        "canonical scheduler human body",
        "second queued human",
        "third queued human",
    ] {
        assert_eq!(input.matches(body).count(), 1);
    }
    assert_eq!(input.matches("[Human \"Owner\"]").count(), 3);
    let native_images: Vec<_> = call["params"]["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "localImage")
        .map(|item| item["path"].as_str().unwrap())
        .collect();
    assert_eq!(native_images, images);
    assert!(crossing.canonical_inputs.pending.is_empty());
    assert_eq!(
        plan.input_refs().unwrap(),
        messages
            .into_iter()
            .map(|message| AcceptedInputRef::Human { message })
            .collect::<Vec<_>>()
    );
    let author = fx.source.current_principal().unwrap().user_id;
    for segment in plan.segments() {
        if let calm_types::conversation::SubmissionRenderSegment::Input {
            input: calm_types::conversation::SubmissionRenderInput::Human { author: frozen, .. },
            ..
        } = segment
        {
            assert_eq!(frozen.registered_user_id, author);
            assert_eq!(frozen.display_name, "Owner");
        }
    }
    assert!(done.canonical_inputs.active.is_none());
    assert!(done.canonical_inputs.pending.is_empty());
    assert_eq!(
        done.canonical_inputs.attempts,
        running.canonical_inputs.attempts
    );
    assert_eq!(done.canonical_inputs.attempts.len(), 1);
}
