use super::*;
use crate::db::prelude::*;
use crate::db::sqlite::conversation::*;
use crate::db::sqlite::{begin_immediate_tx, session_set_harness_observation_runtime_tx};
use crate::harness::input_dispatch::prepare;
use calm_types::conversation::*;
use calm_types::worker::{WorkerSessionId, WorkerSessionState};
use serde_json::json;

mod fixture;
mod local;
mod native;
use fixture::{CONTEXT_BODY, Fixture, HUMAN_BODY, body};

async fn complete(fx: &Fixture) -> RuntimeFrameAfterWrite {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(frame) = complete_once(fx.repo.as_ref(), &fx.backend, &fx.card, &fx.runtime)
                .await
                .unwrap()
            {
                return frame;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn real_end_preserves_input_accepted_after_its_runtime_sample() {
    let fx = Fixture::new().await;
    fx.dispatch().await;
    let sampled = fx.sample().await;
    let observed = fx.observe(&sampled).await;
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &fx.card, &fx.runtime)
        .await
        .unwrap();
    let accepted = crate::conversation_inputs::accept_human_tx(
        &mut tx,
        &fx.source.current_principal().unwrap(),
        &mut carrier,
        body("later pending"),
    )
    .await
    .unwrap();
    let before = carrier.checkpoint().clone();
    let frame = consume_tx(&mut tx, &fx.backend.provider(), &sampled, &observed)
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    let after = fx.checkpoint().await;
    assert_eq!(after.pending, before.pending);
    assert_eq!(after.next_queue_position, before.next_queue_position);
    assert_eq!(
        after.pending[0].accepted_input_ref().unwrap(),
        accepted.input_ref
    );
    assert_eq!(after.attempts, before.attempts);
    assert!(after.active.is_none());
    assert_eq!(frame.snapshot["phase"], "turn_completed");
    fx.stop().await;
}

#[tokio::test]
async fn real_end_whole_header_cas_cannot_rollback_same_phase_budget_warning() {
    let fx = Fixture::new().await;
    fx.dispatch().await;
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &fx.card, &fx.runtime)
        .await
        .unwrap();
    let expected = RuntimeExecutionFence::from_carrier(&carrier);
    let mut header = expected.header().clone();
    header["turn_budget"] =
        serde_json::to_value(calm_types::harness::budget::FrozenBudget::Planner {
            budget: calm_types::harness::budget::PlannerTurnBudget::try_from(10).unwrap(),
            start_attempt: calm_types::harness::snapshot::QueueEntryId::from_wire(
                "budget-owner".into(),
            ),
            warning_attempted: false,
            context_usage: calm_types::harness::budget::FrozenContextUsage::RenderPlan,
        })
        .unwrap();
    persist_runtime_frame_tx(&mut tx, &fx.card, &fx.runtime, &expected, header)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let sampled = fx.sample().await;
    let observed = fx.observe(&sampled).await;
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let mut header = sampled.fence.header().clone();
    header["turn_budget"]["warning_attempted"] = json!(true);
    persist_runtime_frame_tx(&mut tx, &fx.card, &fx.runtime, &sampled.fence, header)
        .await
        .unwrap();
    assert!(
        consume_tx(&mut tx, &fx.backend.provider(), &sampled, &observed)
            .await
            .unwrap()
            .is_none()
    );
    let carrier = current_input_carrier_tx(&mut tx, &fx.card, &fx.runtime)
        .await
        .unwrap();
    assert_eq!(carrier.snapshot()["turn_budget"]["warning_attempted"], true);
    assert!(carrier.checkpoint().active.is_some());
    tx.commit().await.unwrap();
    let frame = complete(&fx).await;
    assert_eq!(frame.snapshot["turn_budget"]["warning_attempted"], true);
    fx.stop().await;
}

#[tokio::test]
async fn real_end_and_idle_projection_rollback_together_without_a_publishable_result() {
    let fx = Fixture::new().await;
    let report = fx.dispatch().await;
    let turn = report.report.outcome().as_ref().unwrap().turn_id();
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &fx.card, &fx.runtime)
        .await
        .unwrap();
    let expected = RuntimeExecutionFence::from_carrier(&carrier);
    let mut header = expected.header().clone();
    header["phase"] = json!("turn_running");
    header["last_turn_id"] = json!(turn);
    persist_runtime_frame_tx(&mut tx, &fx.card, &fx.runtime, &expected, header)
        .await
        .unwrap();
    session_set_harness_observation_runtime_tx(
        &mut tx,
        &fx.runtime,
        WorkerSessionState::Running,
        Some(carrier.snapshot()["last_thread_id"].as_str().unwrap()),
        Some(&turn),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let sampled = fx.sample().await;
    let observed = fx.observe(&sampled).await;
    let card = fx.card.clone();
    let runtime = fx.runtime.clone();
    let provider = fx.backend.provider();
    let published: crate::error::Result<Option<RuntimeFrameAfterWrite>> =
        write_in_tx_typed(fx.repo.as_ref(), move |tx| {
            Box::pin(async move {
                assert!(
                    consume_tx(tx, &provider, &sampled, &observed)
                        .await?
                        .is_some()
                );
                Err(crate::error::CalmError::Conflict(
                    "forced enclosing operation rollback".into(),
                ))
            })
        })
        .await;
    assert!(published.is_err());
    let row = fx
        .repo
        .session_get(&WorkerSessionId(runtime.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, WorkerSessionState::Running);
    assert_eq!(row.active_turn_id.as_deref(), Some(turn.as_str()));
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
        .await
        .unwrap();
    assert_eq!(carrier.snapshot()["phase"], "turn_running");
    assert!(carrier.checkpoint().active.is_some());
    tx.commit().await.unwrap();
    complete(&fx).await;
    let row = fx
        .repo
        .session_get(&WorkerSessionId(runtime))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, WorkerSessionState::Idle);
    assert!(row.active_turn_id.is_none());
    fx.stop().await;
}

#[tokio::test]
async fn end_never_upgrades_no_attempt_or_crossing_marker_into_a_settled_report() {
    let fx = Fixture::new().await;
    assert!(
        complete_once(fx.repo.as_ref(), &fx.backend, &fx.card, &fx.runtime)
            .await
            .unwrap()
            .is_none()
    );
    let prepared = prepare(
        fx.repo.as_ref(),
        &fx.source,
        &fx.card,
        &fx.runtime,
        &fx.submission,
        fx.backend.provider(),
        FrozenInputInvocation {
            model: None,
            effort: None,
            permission: calm_types::harness::PlannerPermissionMode::Never,
        },
    )
    .await
    .unwrap();
    assert!(
        complete_once(fx.repo.as_ref(), &fx.backend, &fx.card, &fx.runtime)
            .await
            .unwrap()
            .is_none()
    );
    let checkpoint = fx.checkpoint().await;
    assert_eq!(
        checkpoint.attempt(&fx.submission).unwrap().attempt,
        *prepared.proof.attempt()
    );
    assert!(matches!(
        checkpoint.attempt(&fx.submission).unwrap().delivery,
        InputDeliveryFact::CrossingUnknown
    ));
    assert!(checkpoint.active.is_some());
    fx.stop().await;
}

#[tokio::test]
async fn end_uses_original_proof_after_private_payload_erasure_without_hydrating_it() {
    let fx = Fixture::new().await;
    fx.dispatch().await;
    let sampled = fx.sample().await;
    fx.observe(&sampled).await;
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let operation = PrivateHistoryOperationId::parse(&crate::model::new_id()).unwrap();
    // Model an already-persisted erasure fact; there is no canonical erase command.
    sqlx::query("UPDATE conversation_message_revisions SET payload_json=?1 WHERE message_id=?2")
        .bind(
            serde_json::to_string(
                &PrivateHistoryPayload::<HumanPayload>::ErasedByPrivateHistoryOperation {
                    operation_id: operation.clone(),
                },
            )
            .unwrap(),
        )
        .bind((fx.message.message_id).as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    // Persist tombstones at this fixture's known payload positions, retaining all identities.
    let tombstone = serde_json::to_string(
        &PrivateHistoryPayload::<HumanPayload>::ErasedByPrivateHistoryOperation {
            operation_id: operation.clone(),
        },
    )
    .unwrap();
    sqlx::query(
        "UPDATE conversation_input_submissions SET render_plan_json=
         json_set(render_plan_json,'$.segments[1].payload',json(?1)) WHERE id=?2",
    )
    .bind(tombstone)
    .bind(fx.submission.as_str())
    .execute(&mut **tx)
    .await
    .unwrap();
    let stored = submission_plan_tx(&mut tx, &fx.submission).await.unwrap();
    assert!(
        SubmissionDispatchProof::from_stored_plan(
            sampled.original.proof.attempt().clone(),
            &stored.plan,
            stored.original_digest.clone()
        )
        .is_err()
    );
    let carrier = current_input_carrier_tx(&mut tx, &fx.card, &fx.runtime)
        .await
        .unwrap();
    let original = original_input_execution_tx(&mut tx, &carrier)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(original.proof, sampled.original.proof);
    tx.commit().await.unwrap();
    complete(&fx).await;
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    assert!(matches!(
        revision_tx(&mut tx, &fx.message).await.unwrap().payload,
        PrivateHistoryPayload::ErasedByPrivateHistoryOperation { .. }
    ));
    let stored = submission_plan_tx(&mut tx, &fx.submission).await.unwrap();
    let encoded = serde_json::to_string(&stored.plan).unwrap();
    assert!(!encoded.contains(CONTEXT_BODY));
    assert!(!encoded.contains(HUMAN_BODY));
    tx.commit().await.unwrap();
    fx.stop().await;
}

#[tokio::test]
async fn actual_end_from_another_owner_cannot_close_the_current_logical_execution() {
    let a = Fixture::new().await;
    let b = Fixture::new().await;
    a.dispatch().await;
    b.dispatch().await;
    let sampled_a = a.sample().await;
    let observed_a = a.observe(&sampled_a).await;
    let sampled_b = b.sample().await;
    let before = b.checkpoint().await;
    let mut tx = begin_immediate_tx(b.repo.db()).await.unwrap();
    assert!(
        consume_tx(&mut tx, &b.backend.provider(), &sampled_b, &observed_a)
            .await
            .unwrap()
            .is_none()
    );
    tx.commit().await.unwrap();
    assert_eq!(b.checkpoint().await, before);
    complete(&a).await;
    complete(&b).await;
    a.stop().await;
    b.stop().await;
}
