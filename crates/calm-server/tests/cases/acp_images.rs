use super::acp_compact::turn;
use super::*;
use crate::support::planner_queue_fixture::{post_input_with_attachments, upload_png};
use base64::Engine as _;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn antigravity_dispatches_bound_image_bytes_through_the_existing_attachment_route() {
    let root = Root::new("unused");
    let stack = boot_as(
        &root,
        "antigravity",
        json!({"ACP_FIXTURE_AGENT_NAME":"Antigravity ACP Server", "ACP_FIXTURE_VERSION":"1.5.0", "ACP_FIXTURE_IMAGES":"1"}),
        "1.5.0",
    ).await;
    let (_, card) = stack
        .create_canonical_track_with(json!({"planner_provider":"antigravity"}))
        .await;
    let bytes = b"\x89PNG\r\n\x1a\nimage payload";
    let (status, uploaded) = upload_png(stack.app.clone(), &card, &bytes[8..]).await;
    assert_eq!(status, StatusCode::CREATED, "{uploaded}");
    let attachment = uploaded["attachmentId"].as_str().unwrap().to_owned();
    let (status, body) = post_input_with_attachments(
        stack.app.clone(),
        &card,
        "Describe this image",
        &[attachment],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        stack.wait_outcomes(&card, 1).await[0]["status"],
        "completed"
    );
    let prompts = requests(&root, "session/prompt");
    let blocks = prompts[0]["params"]["prompt"].as_array().unwrap();
    let image = blocks
        .iter()
        .find(|block| block["type"] == "image")
        .unwrap();
    assert_eq!(image["mimeType"], "image/png");
    let stored: String =
        sqlx::query_scalar("SELECT input_json FROM acp_submissions WHERE worker_session_id=?")
            .bind(stack.runtime(&card).await.id)
            .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
    assert!(matches!(
        calm_server::db::sqlite::AcpSubmissionInput::decode(&stored).unwrap(),
        calm_server::db::sqlite::AcpSubmissionInput::Canonical(_)
    ));
    let messages: i64 =
        sqlx::query_scalar("SELECT count(*) FROM conversation_messages WHERE card_id=?")
            .bind(&card)
            .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
    assert_eq!(messages, 1);
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(image["data"].as_str().unwrap())
            .unwrap(),
        bytes
    );
    assert_eq!(
        turn(&stack, &card, "continue without an image", 2).await["status"],
        "completed"
    );
    assert_eq!(
        requests(&root, "session/prompt")
            .iter()
            .map(|r| r["_pid"].clone())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        1
    );
    stack.shutdown().await;
}

async fn rejected_image_does_not_create_a_receipt(images: bool, sizes: &[usize]) {
    let root = Root::new("unused");
    let stack = boot_as(&root, "antigravity", json!({
        "ACP_FIXTURE_AGENT_NAME":"Antigravity ACP Server", "ACP_FIXTURE_VERSION":"1.5.0",
        "ACP_FIXTURE_IMAGES": if images { "1" } else { "0" }, "ACP_FIXTURE_CAPTURE_SHARED_TOKEN":"1",
    }), "1.5.0").await;
    let (_, card) = stack
        .create_canonical_track_with(json!({"planner_provider":"antigravity"}))
        .await;
    let mut attachments = Vec::new();
    for size in sizes {
        let (status, body) = upload_png(stack.app.clone(), &card, &vec![b'a'; *size]).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        attachments.push(body["attachmentId"].as_str().unwrap().to_owned());
    }
    let (status, body) =
        post_input_with_attachments(stack.app.clone(), &card, "look", &attachments).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entry = body["entry_id"].as_str().unwrap();
    let (runtime, frame) = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let runtime = stack.runtime(&card).await;
            let frame: calm_types::harness::snapshot::CanonicalHarnessSnapshot =
                serde_json::from_value(runtime.handle_state_json.clone().unwrap()).unwrap();
            if frame.canonical_inputs.attempts.iter().any(|attempt| {
                matches!(
                    attempt.delivery,
                    calm_types::conversation::InputDeliveryFact::NotDispatched { .. }
                )
            }) {
                break (runtime, frame);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("canonical NotDispatched fact");
    assert!(frame.canonical_inputs.active.is_none());
    let revision = frame
        .canonical_inputs
        .pending
        .iter()
        .find_map(|pending| match pending {
            calm_types::conversation::PendingInput::Human { message, .. }
                if message.message_id.as_str() == entry =>
            {
                Some(message.revision.get())
            }
            _ => None,
        })
        .expect("original Human input remains editable");
    let native = stack
        .runtime(&card)
        .await
        .session_id
        .expect("attached session");
    assert!(requests(&root, "session/prompt").is_empty());
    let receipts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM acp_submissions WHERE worker_session_id=?")
            .bind(&runtime.id)
            .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
    assert_eq!(receipts, 0);
    let (status, body) = stack
        .send(
            "DELETE",
            &format!("/api/cards/{card}/conversation/messages/{entry}"),
            Some(json!({"carrier_id":runtime.id,"if_revision":revision})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, sibling) = stack
        .create_canonical_track_with(json!({"planner_provider":"antigravity"}))
        .await;
    assert_eq!(
        turn(&stack, &sibling, "sibling continues", 1).await["status"],
        "completed"
    );
    assert_eq!(
        turn(&stack, &card, "continue with text", 1).await["status"],
        "completed"
    );
    assert_eq!(
        stack.runtime(&card).await.session_id.as_deref(),
        Some(native.as_str())
    );
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn antigravity_image_preflight_refuses_large_image_without_poisoning_sessions() {
    rejected_image_does_not_create_a_receipt(true, &[6 * 1024 * 1024]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn antigravity_image_preflight_refuses_large_batch_without_poisoning_sessions() {
    rejected_image_does_not_create_a_receipt(true, &[3 * 1024 * 1024, 3 * 1024 * 1024]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn antigravity_image_preflight_refuses_unnegotiated_images_without_poisoning_sessions() {
    rejected_image_does_not_create_a_receipt(false, &[20]).await;
}
