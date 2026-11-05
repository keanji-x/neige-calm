//! Every image-capable canonical Planner uses the same Human acceptance owner.
use super::*;

#[tokio::test]
async fn canonical_planner_images_bind_before_acceptance_and_replay_the_original_ack() {
    for provider in canonical_planner_providers().filter(|provider| {
        crate::harness::PlannerBackend::metadata(provider)
            .capabilities
            .images
    }) {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let area = post(
            &app,
            &cookie,
            "/api/areas",
            "area",
            json!({"name":"Canonical image", "color":"#123456"}),
        )
        .await;
        let track = post(&app, &cookie, "/api/tracks", "track", json!({"area_id":area["id"], "title":"Image input", "planner_provider":provider.wire_name(), "theme":{"fg":[255,255,255],"bg":[0,0,0]}})).await;
        let (card, _, _) = fixture.carrier(track["id"].as_str().unwrap()).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/cards/{card}/planner/attachments"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::from(b"\x89PNG\r\n\x1a\nimage bytes".to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let upload: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let run = super::presentation_read_feedback_tests::read_run(&app, &cookie, &card).await;
        assert_eq!(run["attachments_supported"], true, "{provider:?}: {run}");
        let uri = format!("/api/cards/{card}/planner/input");
        let body = json!({"text":"Describe image", "attachments":[upload["attachmentId"]]});
        let sent = post(&app, &cookie, &uri, "image", body.clone()).await;
        assert_eq!(post(&app, &cookie, &uri, "image", body).await, sent);
        let message = calm_types::conversation::ConversationMessageId::parse(
            sent["entry_id"].as_str().unwrap(),
        )
        .unwrap();
        let db = fixture.repo.db().clone();
        db.writer()
            .with_writer(|session| {
                Box::pin(async move {
                    let mut tx = session.begin_immediate().await.unwrap();
                    let revision = crate::db::sqlite::conversation::revision_tx(
                        &mut tx,
                        &calm_types::conversation::MessageRevisionRef {
                            message_id: message,
                            revision: std::num::NonZeroU32::new(1).unwrap(),
                        },
                    )
                    .await
                    .unwrap();
                    tx.rollback().await.unwrap();
                    let calm_types::conversation::PrivateHistoryPayload::Present { value } =
                        revision.payload
                    else {
                        panic!("present image input")
                    };
                    assert_eq!(value.bound_attachments.len(), 1);
                    assert_eq!(value.text, "Describe image");
                })
            })
            .await
            .unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM conversation_messages WHERE card_id=?")
                .bind(&card)
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap();
        assert_eq!(count, 1);
        fixture.shutdown_process(&state).await;
    }
}
