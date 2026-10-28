//! Facts/barriers only. Positive native END is exercised through real adapters
//! by the server; these tests never manufacture a qualified native observation.
use super::super::{SqlxRepo, begin_immediate_tx};
use super::tests::{body, fixture, plan};
use super::transaction_tests::{provenance, runtime};
use super::*;
use crate::error::{CalmError, Result};
use calm_types::conversation::*;
use calm_types::organization::LEGACY_OWNER_USER_ID;
use calm_types::runtime::AgentProvider;
use serde_json::json;

async fn original(repo: &SqlxRepo, card: &str, runtime: &str) -> InputAttempt {
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, card, runtime)
        .await
        .unwrap();
    let accepted =
        accept_pending_human_tx(&mut tx, &mut carrier, LEGACY_OWNER_USER_ID, body("initial"))
            .await
            .unwrap();
    let AcceptedInputRef::Human { message } = accepted.input_ref else {
        panic!("human")
    };
    let initial = claim_initial_submission_tx(&mut tx, &mut carrier, plan(message), &provenance())
        .await
        .unwrap();
    let attempt = InputAttempt {
        submission_id: initial.submission_id,
        attempt_id: InputAttemptId::parse(&crate::model::new_id()).unwrap(),
    };
    begin_input_attempt_tx(
        &mut tx,
        &mut carrier,
        attempt.clone(),
        correlation(),
        FrozenInputInvocation {
            model: None,
            effort: None,
            permission: calm_types::harness::PlannerPermissionMode::Never,
        },
    )
    .await
    .unwrap();
    record_input_attempt_fact_tx(
        &mut tx,
        &mut carrier,
        InputAttemptFact {
            attempt: attempt.clone(),
            correlation: correlation(),
            delivery: InputDeliveryFact::Unknown,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    attempt
}

fn correlation() -> InputCorrelationFact {
    InputCorrelationFact {
        provider: AgentProvider::Codex,
        thread_id: "thread".into(),
        native_session_id: None,
        native_turn_id: Some("native-original".into()),
        local_turn_id: None,
        rpc_request_id: None,
    }
}

#[tokio::test]
async fn original_end_preserves_claims_and_pending_when_closing() {
    let (repo, _, card) = fixture().await;
    let runtime = runtime(&repo, &card).await;
    let initial = original(&repo, &card, &runtime).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    let fence = RuntimeExecutionFence::from_carrier(&carrier);
    let mut header = fence.header().clone();
    header["phase"] = json!("turn_running");
    header["last_turn_id"] = json!("native-original");
    persist_runtime_frame_tx(&mut tx, &card, &runtime, &fence, header)
        .await
        .unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    accept_pending_human_tx(
        &mut tx,
        &mut carrier,
        LEGACY_OWNER_USER_ID,
        body("later pending"),
    )
    .await
    .unwrap();
    let expected = carrier.checkpoint().clone();
    let actual = original_input_execution_tx(&mut tx, &carrier)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(actual.proof.attempt(), &initial);
    let fence = RuntimeExecutionFence::from_carrier(&carrier);
    assert!(
        complete_input_execution_tx(&mut tx, &card, &runtime, &fence, &actual)
            .await
            .unwrap()
            .is_some()
    );
    let current = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    let mut expected = expected;
    expected.active = None;
    assert_eq!(current.checkpoint(), &expected);
    let claims: i64 =
        sqlx::query_scalar("SELECT count(*) FROM conversation_submission_inputs WHERE effective=1")
            .fetch_one(&mut **tx)
            .await
            .unwrap();
    assert_eq!(claims, 1);
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn original_end_refuses_missing_and_ambiguous_committed_execution_tuples() {
    for (native, session, local) in [
        (None, None, None),
        (None, None, Some("local")),
        (Some("native"), Some("session"), Some("local")),
    ] {
        let (repo, _, card) = fixture().await;
        let runtime = runtime(&repo, &card).await;
        let attempt = original(&repo, &card, &runtime).await;
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        // A raw historical fact can be incomplete/ambiguous; it is never repaired
        // from current last_turn_id or another native slot by the END getter.
        let mut carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
            .await
            .unwrap();
        let mut frame = carrier.snapshot().clone();
        let facts = frame["canonical_inputs"]["attempts"]
            .as_array_mut()
            .unwrap();
        let fact = facts
            .iter_mut()
            .find(|value| value["attempt"]["submission_id"] == attempt.submission_id.as_str())
            .unwrap();
        fact["correlation"]["native_turn_id"] = json!(native);
        fact["correlation"]["native_session_id"] = json!(session);
        fact["correlation"]["local_turn_id"] = json!(local);
        super::super::session_set_handle_state_tx(&mut tx, &runtime, Some(frame))
            .await
            .unwrap();
        carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
            .await
            .unwrap();
        assert!(
            original_input_execution_tx(&mut tx, &carrier)
                .await
                .unwrap()
                .is_none()
        );
        tx.commit().await.unwrap();
    }
}

#[tokio::test]
async fn original_end_keeps_independent_runtime_transitions_for_their_own_owners() {
    for (key, value) in [
        ("phase", json!("wedged")),
        ("phase", json!("issuing_interrupt")),
        (
            "pending_rewind",
            json!({"provider":"codex_v2","thread_id":"thread","before_turn_id":"prior","reverted":false}),
        ),
        (
            "interruption_intent",
            serde_json::to_value(
                calm_types::harness::HarnessInterruptionIntent::from_request(
                    "native-original",
                    "max_turn_duration",
                )
                .unwrap(),
            )
            .unwrap(),
        ),
        (
            "wedged_reason",
            json!("other owning transition remains unresolved"),
        ),
    ] {
        let (repo, _, card) = fixture().await;
        let runtime = runtime(&repo, &card).await;
        original(&repo, &card, &runtime).await;
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        let carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
            .await
            .unwrap();
        let fence = RuntimeExecutionFence::from_carrier(&carrier);
        let mut header = fence.header().clone();
        header[key] = value;
        persist_runtime_frame_tx(&mut tx, &card, &runtime, &fence, header)
            .await
            .unwrap();
        let current = current_input_carrier_tx(&mut tx, &card, &runtime)
            .await
            .unwrap();
        let original = original_input_execution_tx(&mut tx, &current)
            .await
            .unwrap()
            .unwrap();
        let expected = RuntimeExecutionFence::from_carrier(&current);
        assert!(
            complete_input_execution_tx(&mut tx, &card, &runtime, &expected, &original)
                .await
                .unwrap()
                .is_none(),
            "{key}"
        );
        let after = current_input_carrier_tx(&mut tx, &card, &runtime)
            .await
            .unwrap();
        assert_eq!(after.snapshot(), current.snapshot());
        tx.commit().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_claude_original_local_execution_needs_no_native_session() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let (_, card) = super::tests::fixture_card_for_provider(&repo, AgentProvider::Claude).await;
    let runtime = crate::model::new_id();
    let thread = uuid::Uuid::new_v4().to_string();
    let local = uuid::Uuid::new_v4().to_string();
    let mut frame = calm_types::harness::snapshot::CanonicalHarnessSnapshot::dormant(0);
    frame.header.last_thread_id = Some(thread.clone());
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    super::super::session_start_runtime_tx(
        &mut tx,
        crate::session_projection_repo::WorkerSessionInit::shared_planner(
            runtime.clone(),
            card.clone(),
            AgentProvider::Claude,
            calm_types::worker::WorkerSessionState::Idle,
            Some(thread.clone()),
            serde_json::to_value(frame).unwrap(),
            0,
        ),
    )
    .await
    .unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    let accepted = accept_pending_human_tx(
        &mut tx,
        &mut carrier,
        LEGACY_OWNER_USER_ID,
        body("local initial"),
    )
    .await
    .unwrap();
    let AcceptedInputRef::Human { message } = accepted.input_ref else {
        panic!("human")
    };
    let provenance = ConversationExecutionProvenance::LegacySingleUserDeployment {
        deployment_owner_user_id: LEGACY_OWNER_USER_ID.into(),
        provider: AgentProvider::Claude,
        profile: calm_types::harness::HarnessProfile::Planner,
    };
    let initial = claim_initial_submission_tx(&mut tx, &mut carrier, plan(message), &provenance)
        .await
        .unwrap();
    let attempt = InputAttempt {
        submission_id: initial.submission_id,
        attempt_id: InputAttemptId::parse(&crate::model::new_id()).unwrap(),
    };
    let mut correlation = InputCorrelationFact {
        provider: AgentProvider::Claude,
        thread_id: thread,
        native_session_id: None,
        native_turn_id: None,
        local_turn_id: None,
        rpc_request_id: None,
    };
    begin_input_attempt_tx(
        &mut tx,
        &mut carrier,
        attempt.clone(),
        correlation.clone(),
        FrozenInputInvocation {
            model: None,
            effort: None,
            permission: calm_types::harness::PlannerPermissionMode::Never,
        },
    )
    .await
    .unwrap();
    correlation.local_turn_id = Some(local.clone());
    record_input_attempt_fact_tx(
        &mut tx,
        &mut carrier,
        InputAttemptFact {
            attempt,
            correlation,
            delivery: InputDeliveryFact::Unknown,
        },
    )
    .await
    .unwrap();
    let fence = RuntimeExecutionFence::from_carrier(&carrier);
    let mut header = fence.header().clone();
    header["phase"] = json!("issuing_turn");
    persist_runtime_frame_tx(&mut tx, &card, &runtime, &fence, header)
        .await
        .unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    let original = original_input_execution_tx(&mut tx, &carrier)
        .await
        .unwrap()
        .expect("local execution before init");
    assert_eq!(original.identity.execution, provenance);
    assert_eq!(original.turn_id(), Some(local.as_str()));
    assert!(original.correlation.native_session_id.is_none());
    let fence = RuntimeExecutionFence::from_carrier(&carrier);
    assert!(
        complete_input_execution_tx(&mut tx, &card, &runtime, &fence, &original)
            .await
            .unwrap()
            .is_some()
    );
    tx.commit().await.unwrap();
}

/// The committed shape a Stop deadline (or another failure) leaves behind: the
/// original still active, its Stop intent retained and the row `failed`.
async fn wedged_after_stop(repo: &SqlxRepo, card: &str, runtime: &str, reason: &str) {
    original(repo, card, runtime).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let track: String = sqlx::query_scalar("SELECT track_id FROM worker_sessions WHERE id=?1")
        .bind(runtime)
        .fetch_one(&mut **tx)
        .await
        .unwrap();
    let carrier = current_input_carrier_tx(&mut tx, card, runtime)
        .await
        .unwrap();
    let fence = RuntimeExecutionFence::from_carrier(&carrier);
    let mut header = fence.header().clone();
    header["phase"] = json!("wedged");
    header["last_turn_id"] = json!("native-original");
    header["wedged_reason"] = json!(reason);
    header["interruption_intent"] =
        serde_json::to_value(calm_types::harness::HarnessInterruptionIntent::Control(
            calm_types::harness::HarnessControlIntent::UserStopV1(
                calm_types::harness::HarnessStopRequest {
                    session_id: runtime.into(),
                    card_id: card.into(),
                    track_id: track,
                    thread_id: "thread".into(),
                    turn_id: "native-original".into(),
                    requested_at_ms: 1,
                },
            ),
        ))
        .unwrap();
    persist_runtime_frame_tx(&mut tx, card, runtime, &fence, header)
        .await
        .unwrap();
    super::super::session_set_harness_observation_runtime_tx(
        &mut tx,
        &runtime.to_owned(),
        calm_types::worker::WorkerSessionState::Failed,
        Some("thread"),
        Some("native-original"),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn late_end(
    repo: &SqlxRepo,
    card: &str,
    runtime: &str,
) -> (Result<Option<RuntimeFrameAfterWrite>>, CurrentInputCarrier) {
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let carrier = current_input_fact_carrier_tx(&mut tx, card, runtime)
        .await
        .unwrap();
    let original = original_input_execution_tx(&mut tx, &carrier)
        .await
        .unwrap()
        .unwrap();
    let fence = RuntimeExecutionFence::from_carrier(&carrier);
    let result = complete_input_execution_tx(&mut tx, card, runtime, &fence, &original).await;
    if result.is_ok() {
        tx.commit().await.unwrap();
    } else {
        tx.rollback().await.unwrap();
    }
    (result, carrier)
}

async fn row_state(repo: &SqlxRepo, runtime: &str) -> String {
    sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id=?1")
        .bind(runtime)
        .fetch_one(repo.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn stop_deadline_late_end_restores_only_an_interrupt_timeout_carrier() {
    for (reason, restored) in [
        (calm_types::harness::HARNESS_INTERRUPT_TIMEOUT_REASON, true),
        (calm_types::harness::HARNESS_SYSTEM_ERROR_REASON, false),
        (calm_types::harness::HARNESS_RUNTIME_FAILED_REASON, false),
    ] {
        let (repo, _, card) = fixture().await;
        let runtime = runtime(&repo, &card).await;
        wedged_after_stop(&repo, &card, &runtime, reason).await;
        let (result, _) = late_end(&repo, &card, &runtime).await;
        let done = result.unwrap().expect("the exact original END closes");
        assert!(done.snapshot["canonical_inputs"]["active"].is_null());
        assert_eq!(done.snapshot["phase"], "turn_completed", "{reason}");
        assert!(done.snapshot["interruption_intent"].is_null());
        assert_eq!(
            done.snapshot["wedged_reason"].is_null(),
            restored,
            "{reason}"
        );
        assert_eq!(
            row_state(&repo, &runtime).await,
            if restored { "idle" } else { "failed" },
            "{reason}"
        );
    }
}

#[tokio::test]
async fn stop_deadline_late_end_refuses_a_terminated_failed_carrier_whole() {
    let (repo, _, card) = fixture().await;
    let runtime = runtime(&repo, &card).await;
    wedged_after_stop(
        &repo,
        &card,
        &runtime,
        calm_types::harness::HARNESS_INTERRUPT_TIMEOUT_REASON,
    )
    .await;
    // Another owner already terminated this failed row; an END cannot revive it.
    sqlx::query("UPDATE worker_sessions SET completed_at_ms=1 WHERE id=?1")
        .bind(&runtime)
        .execute(repo.pool())
        .await
        .unwrap();
    let (result, before) = late_end(&repo, &card, &runtime).await;
    assert!(matches!(
        result,
        Err(CalmError::Core(calm_types::error::CoreError::Conflict(_)))
    ));
    assert_eq!(row_state(&repo, &runtime).await, "failed");
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let after = current_input_fact_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    assert_eq!(
        after.snapshot(),
        before.snapshot(),
        "the whole END rolls back"
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn stop_deadline_late_end_after_successor_carrier_changes_nothing() {
    let (repo, _, card) = fixture().await;
    let old = runtime(&repo, &card).await;
    wedged_after_stop(
        &repo,
        &card,
        &old,
        calm_types::harness::HARNESS_INTERRUPT_TIMEOUT_REASON,
    )
    .await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let carrier = current_input_fact_carrier_tx(&mut tx, &card, &old)
        .await
        .unwrap();
    let original = original_input_execution_tx(&mut tx, &carrier)
        .await
        .unwrap()
        .unwrap();
    let fence = RuntimeExecutionFence::from_carrier(&carrier);
    tx.commit().await.unwrap();
    let successor = runtime(&repo, &card).await;
    let successor_before: (String, String) =
        sqlx::query_as("SELECT state,handle_state_json FROM worker_sessions WHERE id=?1")
            .bind(&successor)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    assert!(
        complete_input_execution_tx(&mut tx, &card, &old, &fence, &original)
            .await
            .is_err(),
        "the old END has no current carrier"
    );
    tx.rollback().await.unwrap();
    let successor_after: (String, String) =
        sqlx::query_as("SELECT state,handle_state_json FROM worker_sessions WHERE id=?1")
            .bind(&successor)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(successor_after, successor_before);
    assert_eq!(row_state(&repo, &old).await, "failed");
}
