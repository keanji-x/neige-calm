//! Narrow turn facts follow real claim, crossing, settlement and retry owners.
use super::super::begin_immediate_tx;
use super::tests::{body, fixture, plan};
use super::transaction_tests::{provenance, runtime};
use super::*;
use calm_types::conversation::*;
use calm_types::organization::LEGACY_OWNER_USER_ID;
use calm_types::runtime::AgentProvider;
use serde_json::json;

async fn facts(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card: &str,
    worker: &str,
) -> Option<(i64, bool)> {
    current_turn_input_tx(tx, card, worker).await.unwrap()
}

fn correlation(known: bool) -> InputCorrelationFact {
    InputCorrelationFact {
        provider: AgentProvider::Codex,
        thread_id: "thread".into(),
        native_session_id: None,
        native_turn_id: known.then(|| "native-original".into()),
        local_turn_id: None,
        rpc_request_id: None,
    }
}
fn invocation() -> FrozenInputInvocation {
    FrozenInputInvocation {
        model: None,
        effort: None,
        permission: calm_types::harness::PlannerPermissionMode::Never,
    }
}
async fn settled(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    carrier: &mut CurrentInputCarrier,
    submission_id: InputSubmissionId,
) -> InputAttempt {
    let attempt = InputAttempt {
        submission_id,
        attempt_id: InputAttemptId::parse(&crate::model::new_id()).unwrap(),
    };
    begin_input_attempt_tx(
        tx,
        carrier,
        attempt.clone(),
        correlation(false),
        invocation(),
    )
    .await
    .unwrap();
    record_input_attempt_fact_tx(
        tx,
        carrier,
        InputAttemptFact {
            attempt: attempt.clone(),
            correlation: correlation(true),
            delivery: InputDeliveryFact::Unknown,
        },
    )
    .await
    .unwrap();
    attempt
}

#[tokio::test]
async fn u9_canonical_turn_owner_requires_settled_correlated_execution() {
    let (repo, _, card) = fixture().await;
    let worker = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    assert_eq!(facts(&mut tx, &card, &worker).await, None);
    let mut carrier = current_input_carrier_tx(&mut tx, &card, &worker)
        .await
        .unwrap();
    let accepted =
        accept_pending_human_tx(&mut tx, &mut carrier, LEGACY_OWNER_USER_ID, body("human"))
            .await
            .unwrap();
    assert_eq!(facts(&mut tx, &card, &worker).await, None);
    let AcceptedInputRef::Human { message } = accepted.input_ref else {
        panic!("human")
    };
    let initial = claim_initial_submission_tx(&mut tx, &mut carrier, plan(message), &provenance())
        .await
        .unwrap();
    assert_eq!(facts(&mut tx, &card, &worker).await, None);
    let attempt = InputAttempt {
        submission_id: initial.submission_id,
        attempt_id: InputAttemptId::parse(&crate::model::new_id()).unwrap(),
    };
    begin_input_attempt_tx(
        &mut tx,
        &mut carrier,
        attempt.clone(),
        correlation(false),
        invocation(),
    )
    .await
    .unwrap();
    assert_eq!(facts(&mut tx, &card, &worker).await, None);
    record_input_attempt_fact_tx(
        &mut tx,
        &mut carrier,
        InputAttemptFact {
            attempt: attempt.clone(),
            correlation: correlation(false),
            delivery: InputDeliveryFact::Unknown,
        },
    )
    .await
    .unwrap();
    assert_eq!(facts(&mut tx, &card, &worker).await, None);
    record_input_attempt_fact_tx(
        &mut tx,
        &mut carrier,
        InputAttemptFact {
            attempt,
            correlation: correlation(true),
            delivery: InputDeliveryFact::Unknown,
        },
    )
    .await
    .unwrap();
    let time: i64 = sqlx::query_scalar("SELECT created_at_ms FROM conversation_turns WHERE id=?1")
        .bind(initial.logical_turn_id.as_str())
        .fetch_one(&mut **tx)
        .await
        .unwrap();
    assert_eq!(facts(&mut tx, &card, &worker).await, Some((time, true)));
    assert!(matches!(
        current_turn_input_tx(&mut tx, &card, "wrong-session").await,
        Err(CalmError::Core(calm_types::error::CoreError::Conflict(_)))
    ));
    assert!(matches!(
        current_turn_input_tx(&mut tx, "wrong-card", &worker).await,
        Err(CalmError::Core(calm_types::error::CoreError::Conflict(_)))
    ));
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn u9_canonical_turn_owner_not_dispatched_and_released() {
    let s = super::retry_storage_tests::seed().await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &s.card, &s.carrier)
        .await
        .unwrap();
    let attempt = carrier
        .checkpoint()
        .attempt(&s.root)
        .unwrap()
        .attempt
        .clone();
    record_input_attempt_fact_tx(
        &mut tx,
        &mut carrier,
        InputAttemptFact {
            attempt: attempt.clone(),
            correlation: correlation(false),
            delivery: InputDeliveryFact::NotDispatched {
                basis: "refused".into(),
            },
        },
    )
    .await
    .unwrap();
    assert_eq!(facts(&mut tx, &s.card, &s.carrier).await, None);
    release_submission_tx(&mut tx, &mut carrier, &attempt)
        .await
        .unwrap();
    assert_eq!(facts(&mut tx, &s.card, &s.carrier).await, None);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn u9_canonical_turn_owner_retired_claims_and_reference_retry() {
    let s = super::retry_storage_tests::seed().await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &s.card, &s.carrier)
        .await
        .unwrap();
    let attempt = carrier
        .checkpoint()
        .attempt(&s.root)
        .unwrap()
        .attempt
        .clone();
    record_input_attempt_fact_tx(
        &mut tx,
        &mut carrier,
        InputAttemptFact {
            attempt,
            correlation: correlation(true),
            delivery: InputDeliveryFact::Unknown,
        },
    )
    .await
    .unwrap();
    assert!(facts(&mut tx, &s.card, &s.carrier).await.unwrap().1);
    let mut request = super::retry_storage_tests::request(&s);
    request.revision = "1".into();
    let decision = super::retry::resolve_retry_tx(
        &mut tx,
        &s.card,
        LEGACY_OWNER_USER_ID,
        "retry",
        &request,
        &provenance(),
    )
    .await
    .unwrap()
    .unwrap();
    // Retry leaves the old active execution and settled correlation in place.
    let mut carrier = current_input_carrier_tx(&mut tx, &s.card, &s.carrier)
        .await
        .unwrap();
    assert!(
        original_input_execution_tx(&mut tx, &carrier)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(facts(&mut tx, &s.card, &s.carrier).await, None);
    let original = original_input_execution_tx(&mut tx, &carrier)
        .await
        .unwrap()
        .unwrap();
    let fence = RuntimeExecutionFence::from_carrier(&carrier);
    complete_input_execution_tx(&mut tx, &s.card, &s.carrier, &fence, &original)
        .await
        .unwrap()
        .unwrap();
    carrier = current_input_carrier_tx(&mut tx, &s.card, &s.carrier)
        .await
        .unwrap();
    let HumanInputResolutionDecision::Retry {
        retry_intent_id, ..
    } = decision
    else {
        panic!("retry")
    };
    let successor = claim_initial_submission_tx(
        &mut tx,
        &mut carrier,
        InitialClaim::ExplicitSuccessor { retry_intent_id },
        &provenance(),
    )
    .await
    .unwrap();
    settled(&mut tx, &mut carrier, successor.submission_id.clone()).await;
    let inline: Option<String> = sqlx::query_scalar(
        "SELECT render_plan_json FROM conversation_input_submissions WHERE id=?1",
    )
    .bind(successor.submission_id.as_str())
    .fetch_one(&mut **tx)
    .await
    .unwrap();
    assert!(inline.is_none());
    let time: i64 = sqlx::query_scalar("SELECT created_at_ms FROM conversation_turns WHERE id=?1")
        .bind(successor.logical_turn_id.as_str())
        .fetch_one(&mut **tx)
        .await
        .unwrap();
    assert_eq!(
        facts(&mut tx, &s.card, &s.carrier).await,
        Some((time, true))
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn u9_canonical_turn_owner_current_binding_and_corruption_propagate() {
    let s = super::retry_storage_tests::seed().await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &s.card, &s.carrier)
        .await
        .unwrap();
    let attempt = carrier
        .checkpoint()
        .attempt(&s.root)
        .unwrap()
        .attempt
        .clone();
    record_input_attempt_fact_tx(
        &mut tx,
        &mut carrier,
        InputAttemptFact {
            attempt,
            correlation: correlation(true),
            delivery: InputDeliveryFact::Unknown,
        },
    )
    .await
    .unwrap();
    let original = carrier.snapshot().clone();
    for superseded in [false, true] {
        sqlx::query("SAVEPOINT session_state")
            .execute(&mut **tx)
            .await
            .unwrap();
        if superseded {
            super::super::session_supersede_active_tx(&mut tx, &s.carrier, crate::model::now_ms())
                .await
                .unwrap();
        } else {
            super::super::session_set_status_tx(
                &mut tx,
                &s.carrier,
                calm_types::worker::WorkerSessionState::Failed,
            )
            .await
            .unwrap();
        }
        assert!(matches!(
            current_turn_input_tx(&mut tx, &s.card, &s.carrier).await,
            Err(CalmError::Core(calm_types::error::CoreError::Conflict(_)))
        ));
        sqlx::query("ROLLBACK TO session_state; RELEASE session_state")
            .execute(&mut **tx)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE cards SET session_id=NULL WHERE id=?1")
        .bind(&s.card)
        .execute(&mut **tx)
        .await
        .unwrap();
    assert!(matches!(
        current_turn_input_tx(&mut tx, &s.card, &s.carrier).await,
        Err(CalmError::Core(calm_types::error::CoreError::Conflict(_)))
    ));
    sqlx::query("UPDATE cards SET session_id=?1 WHERE id=?2")
        .bind(&s.carrier)
        .bind(&s.card)
        .execute(&mut **tx)
        .await
        .unwrap();
    let mut broken = original;
    broken["canonical_inputs"]["active"]["submission_ids"] = json!([]);
    super::super::session_set_handle_state_tx(&mut tx, &s.carrier, Some(broken))
        .await
        .unwrap();
    assert!(
        current_turn_input_tx(&mut tx, &s.card, &s.carrier)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn u9_canonical_turn_owner_missing_initial_and_plan_errors_propagate() {
    let s = super::retry_storage_tests::seed().await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &s.card, &s.carrier)
        .await
        .unwrap();
    let attempt = carrier
        .checkpoint()
        .attempt(&s.root)
        .unwrap()
        .attempt
        .clone();
    record_input_attempt_fact_tx(
        &mut tx,
        &mut carrier,
        InputAttemptFact {
            attempt,
            correlation: correlation(true),
            delivery: InputDeliveryFact::Unknown,
        },
    )
    .await
    .unwrap();
    for sql in [
        "DELETE FROM conversation_turns",
        "DROP VIEW conversation_resolved_submission_plans; \
         CREATE VIEW conversation_resolved_submission_plans AS \
         SELECT id,json_remove(render_plan_json,'$.segments[2]') AS render_plan_json,render_plan_sha256 \
         FROM conversation_input_submissions",
        "DROP VIEW conversation_resolved_submission_plans; \
         CREATE VIEW conversation_resolved_submission_plans AS \
         SELECT id,'{}' AS render_plan_json,render_plan_sha256 \
         FROM conversation_input_submissions",
        "ALTER TABLE conversation_turns RENAME TO unavailable_turn_facts",
    ] {
        sqlx::query("SAVEPOINT corrupt")
            .execute(&mut **tx)
            .await
            .unwrap();
        sqlx::query(sql).execute(&mut **tx).await.unwrap();
        assert!(
            current_turn_input_tx(&mut tx, &s.card, &s.carrier)
                .await
                .is_err(),
            "{sql}"
        );
        sqlx::query("ROLLBACK TO corrupt; RELEASE corrupt")
            .execute(&mut **tx)
            .await
            .unwrap();
    }
    assert!(facts(&mut tx, &s.card, &s.carrier).await.unwrap().1);
    tx.rollback().await.unwrap();
}
