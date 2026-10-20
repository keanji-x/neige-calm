//! Canonical transaction entrances, without a provider/model or a fabricated
//! shared execution grant. Transport proof production is tested by its adapter.
use super::super::{SqlxRepo, begin_immediate_tx, session_start_runtime_tx};
use super::tests::{body, fixture, plan};
use super::*;
use calm_types::conversation::*;
use calm_types::harness::HarnessProfile;
use calm_types::organization::LEGACY_OWNER_USER_ID;
use calm_types::runtime::AgentProvider;
use calm_types::worker::WorkerSessionState;

pub(super) async fn runtime(repo: &SqlxRepo, card: &str) -> String {
    let id = crate::model::new_id();
    let mut frame = calm_types::harness::snapshot::CanonicalHarnessSnapshot::dormant(0);
    frame.header.last_thread_id = Some("thread".into());
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
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
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
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
    let current = human_ref(&accepted);
    let mut stale = current.clone();
    stale.revision = std::num::NonZeroU32::new(2).unwrap();
    assert!(
        claim_initial_submission_tx(&mut tx, &mut carrier, plan(stale), &provenance())
            .await
            .is_err()
    );
    let wrong_kind = SubmissionRenderPlan::V1 {
        segments: vec![SubmissionRenderSegment::Input {
            input_position: 0,
            input: SubmissionRenderInput::System {
                observation_id: SystemObservationId::parse(&crate::model::new_id()).unwrap(),
                payload: PrivateHistoryPayload::Present {
                    value: SystemObservation::try_from(
                        calm_types::observation::Observation::SystemContext {
                            text: "wrong source".into(),
                        },
                    )
                    .unwrap(),
                },
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
async fn canonical_initial_non_delivery_release_preserves_pending_and_newer_queue_order() {
    let (repo, _, card) = fixture().await;
    let runtime = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
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
        body("selected Initial"),
    )
    .await
    .unwrap();
    let initial = claim_initial_submission_tx(
        &mut tx,
        &mut carrier,
        plan(human_ref(&older)),
        &provenance(),
    )
    .await
    .unwrap();
    let dispatch = attempt(&initial.submission_id);
    begin_input_attempt_tx(
        &mut tx,
        &mut carrier,
        dispatch.clone(),
        correlation(),
        FrozenInputInvocation {
            model: None,
            effort: None,
            permission: calm_types::harness::PlannerPermissionMode::Never,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    let newer = accept_pending_human_tx(
        &mut tx,
        &mut carrier,
        LEGACY_OWNER_USER_ID,
        body("accepted after Initial"),
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
        InputAttemptFact {
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
    let expected = vec![older.input_ref, steer.input_ref, newer.input_ref];
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
        vec![0, 1, 2]
    );
    assert_eq!(carrier.checkpoint().next_queue_position, 3);
    assert!(carrier.checkpoint().active.is_none());
    release_submission_tx(&mut tx, &mut carrier, &dispatch)
        .await
        .unwrap();
    assert_eq!(
        carrier.checkpoint().pending.len(),
        3,
        "duplicate release cannot duplicate input"
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_reported_unknown_and_stale_attempts_cannot_release_or_requeue() {
    let (repo, _, card) = fixture().await;
    let runtime = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
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
    begin_input_attempt_tx(
        &mut tx,
        &mut carrier,
        original.clone(),
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
            attempt: original.clone(),
            correlation: correlation(),
            delivery: InputDeliveryFact::Unknown,
        },
    )
    .await
    .unwrap();
    let later = attempt(&claimed.submission_id);
    assert!(
        begin_input_attempt_tx(
            &mut tx,
            &mut carrier,
            later.clone(),
            correlation(),
            FrozenInputInvocation {
                model: None,
                effort: None,
                permission: calm_types::harness::PlannerPermissionMode::Never
            }
        )
        .await
        .is_err()
    );
    for attempt in [original.clone(), later] {
        assert!(
            record_input_attempt_fact_tx(
                &mut tx,
                &mut carrier,
                InputAttemptFact {
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
        .fetch_one(&mut **tx)
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
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
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
    begin_input_attempt_tx(
        &mut tx,
        &mut carrier,
        dispatch.clone(),
        correlation(),
        FrozenInputInvocation {
            model: None,
            effort: None,
            permission: calm_types::harness::PlannerPermissionMode::Never,
        },
    )
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
