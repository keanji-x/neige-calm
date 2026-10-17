//! Canonical transaction entrances, without a provider/model or a fabricated
//! shared execution grant. Transport proof production is tested by its adapter.
use super::super::{
    SqlxRepo, begin_immediate_tx, session_set_handle_state_tx, session_start_runtime_tx,
};
use super::tests::{body, fixture, operation, plan};
use super::*;
use calm_types::conversation::*;
use calm_types::harness::HarnessProfile;
use calm_types::organization::LEGACY_OWNER_USER_ID;
use calm_types::runtime::AgentProvider;
use calm_types::worker::WorkerSessionState;
use serde_json::json;

pub(super) async fn runtime(repo: &SqlxRepo, card: &str) -> String {
    let id = crate::model::new_id();
    let mut frame = calm_types::harness::snapshot::CanonicalHarnessSnapshot::dormant(0);
    frame.header.last_thread_id = Some("thread".into());
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        crate::session_projection_repo::WorkerSessionInit::shared_planner(
            id.clone(),
            card.into(),
            AgentProvider::Codex,
            WorkerSessionState::Idle,
            Some("thread".into()),
            serde_json::to_value(frame).unwrap(),
            0,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}
pub(super) fn provenance() -> ConversationExecutionProvenance {
    ConversationExecutionProvenance::LegacySingleUserDeployment {
        deployment_owner_user_id: LEGACY_OWNER_USER_ID.into(),
        provider: AgentProvider::Codex,
        profile: HarnessProfile::Planner,
    }
}
fn human_ref(accepted: &AcceptedPendingInput) -> MessageRevisionRef {
    let AcceptedInputRef::Human { message } = &accepted.input_ref else {
        panic!("fixture accepts human")
    };
    message.clone()
}
fn attempt(submission_id: &InputSubmissionId) -> InputAttempt {
    InputAttempt {
        submission_id: submission_id.clone(),
        attempt_id: InputAttemptId::parse(&crate::model::new_id()).unwrap(),
    }
}
fn correlation() -> InputCorrelationFact {
    InputCorrelationFact {
        provider: AgentProvider::Codex,
        thread_id: "thread".into(),
        native_session_id: None,
        local_turn_id: None,
        native_turn_id: None,
        rpc_request_id: None,
    }
}

#[tokio::test]
async fn canonical_claim_uses_exact_current_pending_revision_and_source() {
    let (repo, _, card) = fixture().await;
    let runtime = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    let accepted = accept_pending_human_tx(
        &mut tx,
        &mut carrier,
        LEGACY_OWNER_USER_ID,
        body("original"),
    )
    .await
    .unwrap();
    let original = human_ref(&accepted);
    let current = edit_pending_human_tx(
        &mut tx,
        &mut carrier,
        LEGACY_OWNER_USER_ID,
        &original,
        body("current revision"),
    )
    .await
    .unwrap();
    assert!(
        claim_initial_submission_tx(&mut tx, &mut carrier, plan(original), &provenance())
            .await
            .is_err()
    );
    let wrong_kind = SubmissionRenderPlan::V1 {
        segments: vec![SubmissionRenderSegment::Input {
            input_position: 0,
            input: SubmissionRenderInput::Legacy {
                message: current.clone(),
                source_locator: "not a migration source".into(),
                boundary: LegacyInputBoundary::SingleRecordedInput,
            },
        }],
    };
    assert!(
        claim_initial_submission_tx(&mut tx, &mut carrier, wrong_kind, &provenance())
            .await
            .is_err()
    );
    let mut wrong_author = plan(current.clone());
    let SubmissionRenderPlan::V1 { segments } = &mut wrong_author;
    let SubmissionRenderSegment::Input {
        input: SubmissionRenderInput::Human { author, .. },
        ..
    } = &mut segments[0]
    else {
        panic!()
    };
    author.registered_user_id = "alice".into();
    assert!(
        claim_initial_submission_tx(&mut tx, &mut carrier, wrong_author, &provenance())
            .await
            .is_err()
    );
    let claimed =
        claim_initial_submission_tx(&mut tx, &mut carrier, plan(current.clone()), &provenance())
            .await
            .unwrap();
    assert!(carrier.checkpoint().pending.is_empty());
    assert_eq!(
        submission_plan_tx(&mut tx, &claimed.submission_id)
            .await
            .unwrap()
            .plan
            .input_refs()
            .unwrap(),
        vec![AcceptedInputRef::Human { message: current }]
    );
    tx.commit().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_messages")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_input_submissions")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn canonical_supplemental_non_delivery_release_preserves_edited_withdrawn_and_newer_queue_order()
 {
    let (repo, _, card) = fixture().await;
    let runtime = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    let first =
        accept_pending_human_tx(&mut tx, &mut carrier, LEGACY_OWNER_USER_ID, body("Initial"))
            .await
            .unwrap();
    let older = accept_pending_human_tx(
        &mut tx,
        &mut carrier,
        LEGACY_OWNER_USER_ID,
        body("older pending"),
    )
    .await
    .unwrap();
    let steer = accept_pending_human_tx(
        &mut tx,
        &mut carrier,
        LEGACY_OWNER_USER_ID,
        body("selected Supplemental"),
    )
    .await
    .unwrap();
    let initial = claim_initial_submission_tx(
        &mut tx,
        &mut carrier,
        plan(human_ref(&first)),
        &provenance(),
    )
    .await
    .unwrap();
    let initial_attempt = attempt(&initial.submission_id);
    begin_input_attempt_tx(
        &mut tx,
        &mut carrier,
        initial_attempt.clone(),
        correlation(),
    )
    .await
    .unwrap();
    let mut native = correlation();
    native.native_turn_id = Some("native-turn".into());
    record_input_attempt_fact_tx(
        &mut tx,
        &mut carrier,
        InputAttemptCheckpoint {
            attempt: initial_attempt,
            correlation: native,
            delivery: InputDeliveryFact::Accepted {
                basis: "codex_turn_start".into(),
                evidence: NativeInputAcceptance::NativeTurn {
                    native_turn_id: "native-turn".into(),
                },
            },
        },
    )
    .await
    .unwrap();
    // Fixture transition uses the real existing runtime writer; the canonical
    // queue itself is unchanged by the native execution owner's phase update.
    let mut running = carrier.snapshot().clone();
    running["phase"] = json!("turn_running");
    running["last_turn_id"] = json!("native-turn");
    session_set_handle_state_tx(&mut tx, &runtime, Some(running))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    let withdraw = accept_pending_human_tx(
        &mut tx,
        &mut carrier,
        LEGACY_OWNER_USER_ID,
        body("withdraw while Supplemental crosses"),
    )
    .await
    .unwrap();
    let supplemental = claim_supplemental_submission_tx(
        &mut tx,
        &mut carrier,
        LEGACY_OWNER_USER_ID,
        &initial.logical_turn_id,
        "native-turn",
        plan(human_ref(&steer)),
    )
    .await
    .unwrap();
    assert_eq!(supplemental.logical_turn_id, initial.logical_turn_id);
    assert_eq!(supplemental.ordinal, 1);
    let dispatch = attempt(&supplemental.submission_id);
    begin_input_attempt_tx(&mut tx, &mut carrier, dispatch.clone(), correlation())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    let older_current = edit_pending_human_tx(
        &mut tx,
        &mut carrier,
        LEGACY_OWNER_USER_ID,
        &human_ref(&older),
        body("edited while Supplemental crosses"),
    )
    .await
    .unwrap();
    withdraw_pending_human_tx(
        &mut tx,
        &mut carrier,
        LEGACY_OWNER_USER_ID,
        &human_ref(&withdraw),
        &operation(),
    )
    .await
    .unwrap();
    let newer = accept_pending_human_tx(
        &mut tx,
        &mut carrier,
        LEGACY_OWNER_USER_ID,
        body("accepted after Supplemental"),
    )
    .await
    .unwrap();
    assert!(
        release_submission_tx(&mut tx, &mut carrier, &dispatch)
            .await
            .is_err(),
        "crossing uncertainty is never release permission"
    );
    record_input_attempt_fact_tx(
        &mut tx,
        &mut carrier,
        InputAttemptCheckpoint {
            attempt: dispatch.clone(),
            correlation: correlation(),
            delivery: InputDeliveryFact::NotDispatched {
                basis: "transport_before_send".into(),
            },
        },
    )
    .await
    .unwrap();
    release_submission_tx(&mut tx, &mut carrier, &dispatch)
        .await
        .unwrap();
    let expected = vec![
        AcceptedInputRef::Human {
            message: older_current,
        },
        steer.input_ref,
        newer.input_ref,
    ];
    assert_eq!(
        carrier
            .checkpoint()
            .pending
            .iter()
            .map(PendingInput::input_ref)
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        carrier
            .checkpoint()
            .pending
            .iter()
            .map(PendingInput::queue_position)
            .collect::<Vec<_>>(),
        vec![1, 2, 4]
    );
    assert_eq!(carrier.checkpoint().next_queue_position, 5);
    let active = carrier.checkpoint().active.as_ref().unwrap();
    assert_eq!(active.submission_ids, vec![initial.submission_id]);
    assert!(matches!(
        revision_tx(&mut tx, &human_ref(&withdraw))
            .await
            .unwrap()
            .payload,
        PrivateHistoryPayload::ErasedByPrivateHistoryOperation { .. }
    ));
    release_submission_tx(&mut tx, &mut carrier, &dispatch)
        .await
        .unwrap();
    assert_eq!(
        carrier.checkpoint().pending.len(),
        3,
        "duplicate release cannot restore withdrawn or duplicate input"
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_reported_unknown_and_stale_attempts_cannot_release_or_requeue() {
    let (repo, _, card) = fixture().await;
    let runtime = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    let accepted = accept_pending_human_tx(
        &mut tx,
        &mut carrier,
        LEGACY_OWNER_USER_ID,
        body("uncertain dispatch"),
    )
    .await
    .unwrap();
    let claimed = claim_initial_submission_tx(
        &mut tx,
        &mut carrier,
        plan(human_ref(&accepted)),
        &provenance(),
    )
    .await
    .unwrap();
    let original = attempt(&claimed.submission_id);
    begin_input_attempt_tx(&mut tx, &mut carrier, original.clone(), correlation())
        .await
        .unwrap();
    record_input_attempt_fact_tx(
        &mut tx,
        &mut carrier,
        InputAttemptCheckpoint {
            attempt: original.clone(),
            correlation: correlation(),
            delivery: InputDeliveryFact::Unknown,
        },
    )
    .await
    .unwrap();
    let later = attempt(&claimed.submission_id);
    assert!(
        begin_input_attempt_tx(&mut tx, &mut carrier, later.clone(), correlation())
            .await
            .is_err()
    );
    for attempt in [original.clone(), later] {
        assert!(
            record_input_attempt_fact_tx(
                &mut tx,
                &mut carrier,
                InputAttemptCheckpoint {
                    attempt: attempt.clone(),
                    correlation: correlation(),
                    delivery: InputDeliveryFact::NotDispatched {
                        basis: "later_local_refusal".into()
                    }
                }
            )
            .await
            .is_err()
        );
        assert!(
            release_submission_tx(&mut tx, &mut carrier, &attempt)
                .await
                .is_err()
        );
    }
    assert!(carrier.checkpoint().pending.is_empty());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM conversation_submission_inputs WHERE effective=1"
        )
        .fetch_one(&mut *tx)
        .await
        .unwrap(),
        1
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_release_requires_persisted_non_delivery_not_an_uncommitted_cache_fact() {
    let (repo, _, card) = fixture().await;
    let runtime = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    let accepted = accept_pending_human_tx(
        &mut tx,
        &mut carrier,
        LEGACY_OWNER_USER_ID,
        body("still uncertain in storage"),
    )
    .await
    .unwrap();
    let claimed = claim_initial_submission_tx(
        &mut tx,
        &mut carrier,
        plan(human_ref(&accepted)),
        &provenance(),
    )
    .await
    .unwrap();
    let dispatch = attempt(&claimed.submission_id);
    begin_input_attempt_tx(&mut tx, &mut carrier, dispatch.clone(), correlation())
        .await
        .unwrap();
    // Model a cache changed before/after a failed report write. No positive
    // provider fact has been recorded in the authoritative SQLite checkpoint.
    carrier.checkpoint.attempts[0].delivery = InputDeliveryFact::NotDispatched {
        basis: "unpersisted_cache_only".into(),
    };
    assert!(
        release_submission_tx(&mut tx, &mut carrier, &dispatch)
            .await
            .is_err(),
        "a cached fact must not authorize release of a durable Unknown"
    );
    tx.rollback().await.unwrap();
}
