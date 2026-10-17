use super::*;
use crate::db::prelude::*;
use crate::db::sqlite::conversation::*;
use crate::db::sqlite::{begin_immediate_tx, session_set_harness_observation_runtime_tx};
use crate::harness::input_dispatch::{Target, prepare};
use calm_types::conversation::*;
use calm_types::worker::{WorkerSessionId, WorkerSessionState};
use serde_json::json;

mod fixture;
mod native;
use fixture::{CONTEXT_BODY, Fixture, HUMAN_BODY, body, plan};

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
async fn real_local_end_closes_execution_without_releasing_unknown_or_authorizing_next_input() {
    for provider in [AgentProvider::Claude, AgentProvider::OpenCode] {
        let fx = Fixture::new(provider).await;
        let committed = fx.dispatch().await;
        let turn = committed.report.outcome().as_ref().unwrap().clone();
        let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
        let mut carrier = current_input_carrier_tx(&mut tx, &fx.card, &fx.runtime)
            .await
            .unwrap();
        let accepted = crate::conversation_inputs::accept_human_tx(
            &mut tx,
            &fx.source.current_principal().unwrap(),
            &mut carrier,
            body("new accepted input"),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let before = fx.checkpoint().await;
        fx.auth.sessions.remove(&fx.session).unwrap();
        let frame = complete(&fx).await;
        assert_eq!(frame.snapshot["phase"], "turn_completed");
        assert_eq!(frame.snapshot["last_turn_id"], turn);
        let mut expected = before;
        expected.active = None;
        assert_eq!(fx.checkpoint().await, expected);
        assert!(matches!(
            expected.attempt(&fx.submission).unwrap().delivery,
            InputDeliveryFact::Unknown
        ));
        let effective: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM conversation_submission_inputs WHERE submission_id=?1 AND effective=1"
        ).bind(fx.submission.as_str()).fetch_one(fx.repo.pool()).await.unwrap();
        assert_eq!(effective, 1);
        let row = fx
            .repo
            .session_get(&WorkerSessionId(fx.runtime.clone()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, WorkerSessionState::Idle);
        assert!(row.active_turn_id.is_none());
        assert!(
            complete_once(fx.repo.as_ref(), &fx.backend, &fx.card, &fx.runtime)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            !serde_json::to_string(&frame.snapshot)
                .unwrap()
                .contains(HUMAN_BODY)
        );
        assert!(
            !serde_json::to_string(&frame.snapshot)
                .unwrap()
                .contains(CONTEXT_BODY)
        );
        let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
        let mut carrier = current_input_carrier_tx(&mut tx, &fx.card, &fx.runtime)
            .await
            .unwrap();
        let AcceptedInputRef::Human { message } = accepted.input_ref else {
            panic!("human")
        };
        assert!(
            crate::conversation_inputs::claim_human_initial_tx(
                &mut tx,
                &fx.source,
                &mut carrier,
                plan(message),
            )
            .await
            .is_err(),
            "a past END cannot revive the logged-out source"
        );
        tx.rollback().await.unwrap();
        fx.stop().await;
    }
}

#[tokio::test]
async fn real_end_preserves_input_accepted_after_its_runtime_sample() {
    let fx = Fixture::new(AgentProvider::Claude).await;
    fx.dispatch().await;
    let sampled = fx.sample().await;
    let observed = fx.observe(&sampled).await;
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
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
    assert_eq!(after.pending[0].input_ref(), accepted.input_ref);
    assert_eq!(after.attempts, before.attempts);
    assert!(after.active.is_none());
    assert_eq!(frame.snapshot["phase"], "turn_completed");
    fx.stop().await;
}

#[tokio::test]
async fn real_end_whole_header_cas_cannot_rollback_same_phase_budget_warning() {
    let fx = Fixture::new(AgentProvider::Claude).await;
    fx.dispatch().await;
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &fx.card, &fx.runtime)
        .await
        .unwrap();
    let expected = RuntimeExecutionFence::from_carrier(&carrier);
    let mut header = expected.header().clone();
    header["turn_budget"] = json!({"warning_attempted":false,"start_attempt":"budget-owner"});
    persist_runtime_frame_tx(&mut tx, &fx.card, &fx.runtime, &expected, header)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let sampled = fx.sample().await;
    let observed = fx.observe(&sampled).await;
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
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
    let fx = Fixture::new(AgentProvider::OpenCode).await;
    let report = fx.dispatch().await;
    let turn = report.report.outcome().as_ref().unwrap();
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
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
        Some(turn),
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
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
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
    let fx = Fixture::new(AgentProvider::Claude).await;
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
        Target::Initial,
        None,
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
    assert!(!fx.dir.path().join("argv").exists());
    fx.stop().await;
}

#[tokio::test]
async fn end_uses_original_proof_after_private_payload_erasure_without_hydrating_it() {
    let fx = Fixture::new(AgentProvider::Claude).await;
    fx.dispatch().await;
    let sampled = fx.sample().await;
    fx.observe(&sampled).await;
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    let operation = PrivateHistoryOperationId::parse(&crate::model::new_id()).unwrap();
    erase_message_revisions_tx(&mut tx, &fx.message.message_id, &operation)
        .await
        .unwrap();
    erase_submission_render_payloads_tx(&mut tx, &fx.submission, &operation)
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
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
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
    let a = Fixture::new(AgentProvider::Claude).await;
    let b = Fixture::new(AgentProvider::Claude).await;
    a.dispatch().await;
    b.dispatch().await;
    let sampled_a = a.sample().await;
    let observed_a = a.observe(&sampled_a).await;
    let sampled_b = b.sample().await;
    let before = b.checkpoint().await;
    let mut tx = begin_immediate_tx(b.repo.pool()).await.unwrap();
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
