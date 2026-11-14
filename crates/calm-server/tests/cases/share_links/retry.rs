//! Ordinary scoped reads retain retry provenance across successor claims.
use super::fixtures::canonical;
use super::*;
use calm_server::db::sqlite::conversation::*;
use calm_types::conversation::*;

#[tokio::test]
async fn scoped_retry_preserves_system_and_human_history() {
    let fx = Fixture::new().await;
    let (card, worker) = canonical(&fx).await;
    let link = fx.create(true).await;
    let cookie = super::tokens::unlock(&fx, &link).await;
    let id = link["link"]["delegation"]["id"].as_str().unwrap();
    let shared_url = format!("/api/cards/{card}/harness/items?contract=conversation_v1");
    let owner_url = shared_url.clone();
    let db = fx.state.raw_repo().sqlite_db().unwrap();
    let execution = ConversationExecutionProvenance::LegacySingleUserDeployment {
        deployment_owner_user_id: OWNER_USER_ID.into(),
        provider: calm_types::runtime::AgentProvider::Codex,
        profile: calm_types::harness::HarnessProfile::Assistant,
    };
    let request = db
        .writer()
        .with_writer(|session| {
            Box::pin(async {
                let mut tx = session.begin_immediate().await.unwrap();
                // Bind a real persisted event to its System observation through the acceptance owner.
                let event = calm_server::event::Event::AssistantInputRequested {
                    card_id: card.clone().into(),
                    track_id: fx.track.clone().into(),
                    original_worker: worker.clone(),
                    source: calm_types::system_assistant::SystemAssistantSource::TodaySummary,
                    subject: "read fixture".into(),
                    version: 1,
                    text: "Published System".into(),
                };
                let event_id = calm_server::db::sqlite::append_decision_event_in_tx(
                    &mut tx,
                    &calm_server::ids::ActorId::Kernel,
                    &calm_server::event::EventScope::Card {
                        card: card.clone().into(),
                        track: fx.track.clone().into(),
                        area: fx.area.clone().into(),
                    },
                    None,
                    &event,
                )
                .await
                .unwrap();
                let fact = calm_server::db::sqlite::event_inputs::event_input_tx(&mut tx, event_id)
                    .await
                    .unwrap();
                sqlx::query(
                    "UPDATE worker_sessions SET thread_id='thread',
            handle_state_json=json_set(handle_state_json,'$.last_thread_id','thread') WHERE id=?1",
                )
                .bind(&worker)
                .execute(&mut **tx)
                .await
                .unwrap();
                let mut carrier = current_input_carrier_tx(&mut tx, &card, &worker)
                    .await
                    .unwrap();
                accept_pending_system_tx(
                    &mut tx,
                    &mut carrier,
                    SystemObservation::try_from(
                        calm_types::observation::Observation::TaskGateResult {
                            idempotency_key: "private-attempt".into(),
                            key: "private-task".into(),
                            passed: false,
                            failing_step: Some("test".into()),
                            exit_code: Some(101),
                            log_tail: "SECRET cwd=/host/work dirty=[/host/private.txt] <task>internal</task>".into(),
                            attempt: 1,
                            status_detail: None,
                            target: None,
                        },
                    )
                    .unwrap(),
                    &fact,
                )
                .await
                .unwrap();
                let plan = pending_plan_for_prefix_tx(
                    &mut tx,
                    &carrier,
                    std::num::NonZeroU32::new(2).unwrap(),
                    vec![],
                )
                .await
                .unwrap();
                let claim = claim_initial_submission_tx(&mut tx, &mut carrier, plan, &execution)
                    .await
                    .unwrap();
                let attempt_id = InputAttemptId::parse(&"e".repeat(32)).unwrap();
                begin_input_attempt_tx(
                    &mut tx,
                    &mut carrier,
                    InputAttempt {
                        submission_id: claim.submission_id.clone(),
                        attempt_id: attempt_id.clone(),
                    },
                    InputCorrelationFact {
                        provider: calm_types::runtime::AgentProvider::Codex,
                        thread_id: "thread".into(),
                        native_turn_id: None,
                        native_session_id: None,
                        local_turn_id: None,
                        rpc_request_id: None,
                    },
                    FrozenInputInvocation {
                        model: Some("model".into()),
                        effort: Some("high".into()),
                        permission: calm_types::harness::PlannerPermissionMode::Never,
                    },
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
                RetryConversationInputRequest {
                    carrier_id: worker.clone(),
                    submission_id: claim.submission_id.as_str().into(),
                    attempt_id: attempt_id.as_str().into(),
                    revision: "0".into(),
                }
            })
        })
        .await
        .unwrap();
    async fn page(fx: &Fixture, url: &str, cookie: &str, id: &str) -> Value {
        let response = fx
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(url)
                    .header("cookie", cookie)
                    .header("X-Calm-Share-Link", id)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let page: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(status, StatusCode::OK, "{page}");
        page
    }
    let before = page(&fx, &shared_url, &cookie, id).await;
    assert_eq!(before["items"].as_array().unwrap().len(), 2, "{before}");
    assert!(before.to_string().contains("SECRET cwd=/host/work"));
    let retry = db
        .writer()
        .with_writer(|session| {
            Box::pin(async {
                let mut tx = session.begin_immediate().await.unwrap();
                let retry = retry_conversation_input_tx(
                    &mut tx,
                    &card,
                    OWNER_USER_ID,
                    "shared-retry",
                    &request,
                    &execution,
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
                retry
            })
        })
        .await
        .unwrap();
    let (status, retired_owner) = fx
        .request("GET", &owner_url, Some(&fx.cookie), Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(retired_owner["items"].as_array().unwrap().len(), 2);
    assert!(retired_owner.to_string().contains("SECRET cwd=/host/work"));
    assert_eq!(
        page(&fx, &shared_url, &cookie, id).await["items"],
        before["items"]
    );
    db.writer()
        .with_writer(|session| {
            Box::pin(async {
                let mut tx = session.begin_immediate().await.unwrap();
                let mut carrier = current_input_carrier_tx(&mut tx, &card, &worker)
                    .await
                    .unwrap();
                claim_initial_submission_tx(
                    &mut tx,
                    &mut carrier,
                    InitialClaim::ExplicitSuccessor {
                        retry_intent_id: RetryIntentId::parse(&retry.retry_intent_id).unwrap(),
                    },
                    &execution,
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
            })
        })
        .await
        .unwrap();
    let after = page(&fx, &shared_url, &cookie, id).await;
    assert_eq!(
        after["items"], before["items"],
        "same positions and speech return after successor claim"
    );
}
