//! HTTP display regressions across the intervals with no effective consumer.
use super::*;
use crate::db::sqlite::conversation as storage;
use calm_types::conversation::*;

pub(super) async fn system_display(app: &axum::Router, cookie: &str, card: &str) -> Value {
    let page = read(
        app,
        cookie,
        &format!("/api/cards/{card}/harness/items?contract=conversation_v1"),
    )
    .await;
    let systems: Vec<_> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| &item["source"])
        .filter(|source| source["kind"] == "system")
        .collect();
    assert_eq!(
        systems.len(),
        1,
        "one retained presentation, even across repeated claims: {page}"
    );
    assert!(systems[0]["text"].as_str().unwrap().contains(SYSTEM_BODY));
    systems[0].clone()
}

fn provenance() -> ConversationExecutionProvenance {
    ConversationExecutionProvenance::LegacySingleUserDeployment {
        deployment_owner_user_id: calm_types::organization::LEGACY_OWNER_USER_ID.into(),
        provider: calm_types::runtime::AgentProvider::Codex,
        profile: calm_types::harness::HarnessProfile::Planner,
    }
}

pub(super) async fn queued_restore_reopen(flow: Flow, id: RetryIntentId, display: Value) {
    assert_eq!(
        system_display(&flow.app, &flow.cookie, &flow.card).await,
        display,
        "Retry must preserve the original presentation while issuance is paused"
    );
    // Exercise the production claim/fact/release owners, without asking the native
    // peer to cross. The restored intent carries no System payload.
    {
        let mut tx = crate::db::sqlite::begin_immediate_tx(flow.fixture.repo.db())
            .await
            .unwrap();
        let mut carrier = storage::current_input_carrier_tx(&mut tx, &flow.card, &flow.worker)
            .await
            .unwrap();
        let invocation = storage::retry_intent_tx(&mut tx, &carrier, &id)
            .await
            .unwrap()
            .invocation;
        let claimed = storage::claim_initial_submission_tx(
            &mut tx,
            &mut carrier,
            storage::InitialClaim::ExplicitSuccessor {
                retry_intent_id: id.clone(),
            },
            &provenance(),
        )
        .await
        .unwrap();
        let attempt = InputAttempt {
            submission_id: claimed.submission_id,
            attempt_id: InputAttemptId::parse(&crate::model::new_id()).unwrap(),
        };
        let correlation = InputCorrelationFact {
            provider: calm_types::runtime::AgentProvider::Codex,
            thread_id: THREAD.into(),
            native_turn_id: None,
            native_session_id: None,
            local_turn_id: None,
            rpc_request_id: None,
        };
        storage::begin_input_attempt_tx(
            &mut tx,
            &mut carrier,
            attempt.clone(),
            correlation.clone(),
            invocation,
        )
        .await
        .unwrap();
        storage::record_input_attempt_fact_tx(
            &mut tx,
            &mut carrier,
            InputAttemptFact {
                attempt: attempt.clone(),
                correlation,
                delivery: InputDeliveryFact::NotDispatched {
                    basis: "local_preflight".into(),
                },
            },
        )
        .await
        .unwrap();
        storage::release_submission_tx(&mut tx, &mut carrier, &attempt)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            system_display(&flow.app, &flow.cookie, &flow.card).await,
            display,
            "NotDispatched restores only the reference-bearing intent"
        );
    }
    flow.fixture.shutdown_process(&flow.state).await;
    let Flow {
        mut fixture,
        state,
        app,
        _client,
        peer,
        card,
        ..
    } = flow;
    drop((state, app, _client, peer));
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, app, cookie) = fixture.application().await;
    assert_eq!(
        system_display(&app, &cookie, &card).await,
        display,
        "cold recovery has no live Human source or new claim"
    );
    let effective: i64 = sqlx::query_scalar("SELECT count(*) FROM conversation_submission_inputs WHERE observation_id IS NOT NULL AND effective=1")
        .fetch_one(fixture.repo.pool()).await.unwrap();
    assert_eq!(
        effective, 0,
        "reading must never reacquire execution claims"
    );
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn canonical_retry_http_mixed_queued_restore_reopen_is_readable() {
    let mut flow = Flow::running_with_model(true).await;
    flow.state
        .harness
        .get(&flow.worker)
        .unwrap()
        .pause_issuance_for_dev();
    flow.report(SYSTEM_BODY).await;
    flow.human().await;
    flow.end(TURN).await;
    flow.fixture
        .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
        .await;
    let display = system_display(&flow.app, &flow.cookie, &flow.card).await;
    // Mixed batches are supported by the storage claim contract. Use that owner
    // to select both actual HTTP/Dispatcher acceptances, rather than copy a plan.
    let mut tx = crate::db::sqlite::begin_immediate_tx(flow.fixture.repo.db())
        .await
        .unwrap();
    let mut carrier = storage::current_input_carrier_tx(&mut tx, &flow.card, &flow.worker)
        .await
        .unwrap();
    let plan = storage::pending_plan_for_prefix_tx(
        &mut tx,
        &carrier,
        std::num::NonZeroU32::new(2).unwrap(),
        vec![],
    )
    .await
    .unwrap();
    let claimed = storage::claim_initial_submission_tx(&mut tx, &mut carrier, plan, &provenance())
        .await
        .unwrap();
    let attempt = InputAttempt {
        submission_id: claimed.submission_id,
        attempt_id: InputAttemptId::parse(&crate::model::new_id()).unwrap(),
    };
    storage::begin_input_attempt_tx(
        &mut tx,
        &mut carrier,
        attempt.clone(),
        InputCorrelationFact {
            provider: calm_types::runtime::AgentProvider::Codex,
            thread_id: THREAD.into(),
            native_turn_id: None,
            native_session_id: None,
            local_turn_id: None,
            rpc_request_id: None,
        },
        FrozenInputInvocation {
            model: Some("retry-model".into()),
            effort: Some("high".into()),
            permission: calm_types::harness::PlannerPermissionMode::Never,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let uri = format!("/api/cards/{}/conversation/input/retry", flow.card);
    let result = post(
        &flow.app,
        &flow.cookie,
        &uri,
        "mixed-repeat",
        json!({"carrier_id":flow.worker,
        "submission_id":attempt.submission_id,"attempt_id":attempt.attempt_id,"revision":"0"}),
    )
    .await;
    let id = RetryIntentId::parse(result["retry_intent_id"].as_str().unwrap()).unwrap();
    queued_restore_reopen(flow, id, display).await;
}
