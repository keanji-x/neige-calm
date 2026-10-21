//! Real coupled resolutions, ordinary claims/releases, retained facts and reopen.
use super::super::begin_immediate_tx;
use super::retry::{RetryUnavailableReason, resolve_retry_tx};
use super::retry_storage_tests::{request, seed};
use super::transaction_tests::provenance;
use super::*;
use calm_types::conversation::*;
use calm_types::organization::LEGACY_OWNER_USER_ID;

fn id(decision: &HumanInputResolutionDecision) -> RetryIntentId {
    match decision {
        HumanInputResolutionDecision::Keep { .. } => panic!("expected Retry"),
        HumanInputResolutionDecision::Retry {
            retry_intent_id, ..
        } => retry_intent_id.clone(),
    }
}

#[tokio::test]
async fn canonical_retry_reserves_one_successor_and_receipt_atomically() {
    let s = seed().await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    let decision = resolve_retry_tx(
        &mut tx,
        &s.card,
        LEGACY_OWNER_USER_ID,
        "zero-revision",
        &request(&s),
        &provenance(),
    )
    .await
    .unwrap()
    .unwrap();
    let carrier = current_input_fact_carrier_tx(&mut tx, &s.card, &s.carrier)
        .await
        .unwrap();
    assert_eq!(carrier.checkpoint().receipts.len(), 1);
    assert_eq!(carrier.checkpoint().attempt(&s.root).unwrap().revision, 1);
    assert_eq!(carrier.checkpoint().pending.len(), 1);
    assert!(matches!(
        carrier.checkpoint().pending[0],
        PendingInput::ExplicitSuccessor { .. }
    ));
    let intent = retry_intent_tx(&mut tx, &carrier, &id(&decision))
        .await
        .unwrap();
    assert_eq!(intent.plan.plan, s.render);
    assert_eq!(intent.pending, carrier.checkpoint().pending[0]);
    let (submissions, effective, retired): (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM conversation_input_submissions),(SELECT count(*) FROM
        conversation_submission_inputs WHERE effective=1),(SELECT count(*) FROM
        conversation_submission_inputs WHERE retirement_reason='human_resolution' AND
        released_by_attempt_id IS NULL AND retired_by_retry_intent_id=?1)",
    )
    .bind(id(&decision).as_str())
    .fetch_one(&mut **tx)
    .await
    .unwrap();
    assert_eq!((submissions, effective, retired), (1, 0, 2));
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_retry_same_key_after_response_loss_replays_original_successor() {
    let s = seed().await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    let first = resolve_retry_tx(
        &mut tx,
        &s.card,
        LEGACY_OWNER_USER_ID,
        "replay",
        &request(&s),
        &provenance(),
    )
    .await
    .unwrap()
    .unwrap();
    tx.commit().await.unwrap();
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &s.card, &s.carrier)
        .await
        .unwrap();
    claim_initial_submission_tx(
        &mut tx,
        &mut carrier,
        InitialClaim::ExplicitSuccessor {
            retry_intent_id: id(&first),
        },
        &provenance(),
    )
    .await
    .unwrap();
    let replay = resolve_retry_tx(
        &mut tx,
        &s.card,
        LEGACY_OWNER_USER_ID,
        "replay",
        &request(&s),
        &provenance(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(replay, first);
    assert!(
        resolve_retry_tx(
            &mut tx,
            &s.card,
            LEGACY_OWNER_USER_ID,
            "another-key",
            &request(&s),
            &provenance()
        )
        .await
        .is_err()
    );
    assert!(
        keep_conversation_input_tx(
            &mut tx,
            &s.card,
            LEGACY_OWNER_USER_ID,
            "replay",
            &request(&s)
        )
        .await
        .is_err()
    );
    assert_eq!(carrier.checkpoint().pending.len(), 0);
    assert_eq!(carrier.checkpoint().receipts.len(), 1);
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_retry_rollback_and_cross_action_key_conflict() {
    let s = seed().await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    resolve_retry_tx(
        &mut tx,
        &s.card,
        LEGACY_OWNER_USER_ID,
        "rollback",
        &request(&s),
        &provenance(),
    )
    .await
    .unwrap()
    .unwrap();
    tx.rollback().await.unwrap();
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    let carrier = current_input_fact_carrier_tx(&mut tx, &s.card, &s.carrier)
        .await
        .unwrap();
    assert!(carrier.checkpoint().receipts.is_empty());
    assert!(carrier.checkpoint().pending.is_empty());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_input_retry_relations")
            .fetch_one(&mut **tx)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM conversation_submission_inputs WHERE effective=1"
        )
        .fetch_one(&mut **tx)
        .await
        .unwrap(),
        2
    );
    keep_conversation_input_tx(&mut tx, &s.card, LEGACY_OWNER_USER_ID, "keep", &request(&s))
        .await
        .unwrap();
    assert!(
        resolve_retry_tx(
            &mut tx,
            &s.card,
            LEGACY_OWNER_USER_ID,
            "keep",
            &request(&s),
            &provenance()
        )
        .await
        .is_err()
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_retry_not_dispatched_repeated_crossings_preserve_history() {
    let s = seed().await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    let decision = resolve_retry_tx(
        &mut tx,
        &s.card,
        LEGACY_OWNER_USER_ID,
        "repeat",
        &request(&s),
        &provenance(),
    )
    .await
    .unwrap()
    .unwrap();
    let intent_id = id(&decision);
    let mut carrier = current_input_carrier_tx(&mut tx, &s.card, &s.carrier)
        .await
        .unwrap();
    let pending = carrier.checkpoint().pending[0].clone();
    accept_pending_human_tx(
        &mut tx,
        &mut carrier,
        LEGACY_OWNER_USER_ID,
        super::tests::body("ordinary input interleaved after the retry intent"),
    )
    .await
    .unwrap();
    let ordered_pending = carrier.checkpoint().pending.clone();
    assert_eq!(ordered_pending.len(), 2);
    assert_eq!(ordered_pending[0], pending);
    let mut submissions = std::collections::HashSet::new();
    let mut turns = std::collections::HashSet::new();
    for _ in 0..3 {
        let claimed = claim_initial_submission_tx(
            &mut tx,
            &mut carrier,
            InitialClaim::ExplicitSuccessor {
                retry_intent_id: intent_id.clone(),
            },
            &provenance(),
        )
        .await
        .unwrap();
        assert_eq!(claimed.plan, s.render);
        assert!(submissions.insert(claimed.submission_id.clone()));
        assert!(turns.insert(claimed.logical_turn_id.clone()));
        assert_eq!(
            submission_retry_intent_tx(&mut tx, &claimed.submission_id)
                .await
                .unwrap(),
            Some(intent_id.clone())
        );
        let attempt = InputAttempt {
            submission_id: claimed.submission_id,
            attempt_id: InputAttemptId::parse(&crate::model::new_id()).unwrap(),
        };
        let correlation = InputCorrelationFact {
            provider: calm_types::runtime::AgentProvider::Codex,
            thread_id: "thread".into(),
            native_session_id: None,
            native_turn_id: None,
            local_turn_id: None,
            rpc_request_id: None,
        };
        let invocation = retry_intent_tx(&mut tx, &carrier, &intent_id)
            .await
            .unwrap()
            .invocation;
        begin_input_attempt_tx(
            &mut tx,
            &mut carrier,
            attempt.clone(),
            correlation.clone(),
            invocation,
        )
        .await
        .unwrap();
        record_input_attempt_fact_tx(
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
        release_submission_tx(&mut tx, &mut carrier, &attempt)
            .await
            .unwrap();
        assert_eq!(carrier.checkpoint().pending, ordered_pending);
        carrier
            .set_phase(calm_types::harness::HarnessPhaseTag::Idle)
            .unwrap();
        super::carrier::save_input_carrier_tx(&mut tx, &mut carrier)
            .await
            .unwrap();
    }
    assert_eq!(carrier.checkpoint().attempts.len(), 4);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM conversation_submission_inputs WHERE retirement_reason='not_dispatched'
        AND released_by_attempt_id IS NOT NULL AND retired_by_retry_intent_id IS NULL").fetch_one(&mut **tx).await.unwrap(),6);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM conversation_submission_inputs WHERE effective=1"
        )
        .fetch_one(&mut **tx)
        .await
        .unwrap(),
        0
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_retry_unknown_successor_can_have_a_new_human_intent() {
    let s = seed().await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    let first = resolve_retry_tx(
        &mut tx,
        &s.card,
        LEGACY_OWNER_USER_ID,
        "first",
        &request(&s),
        &provenance(),
    )
    .await
    .unwrap()
    .unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &s.card, &s.carrier)
        .await
        .unwrap();
    let intent = retry_intent_tx(&mut tx, &carrier, &id(&first))
        .await
        .unwrap();
    let claimed = claim_initial_submission_tx(
        &mut tx,
        &mut carrier,
        InitialClaim::ExplicitSuccessor {
            retry_intent_id: id(&first),
        },
        &provenance(),
    )
    .await
    .unwrap();
    let attempt = InputAttempt {
        submission_id: claimed.submission_id.clone(),
        attempt_id: InputAttemptId::parse(&crate::model::new_id()).unwrap(),
    };
    begin_input_attempt_tx(
        &mut tx,
        &mut carrier,
        attempt.clone(),
        InputCorrelationFact {
            provider: calm_types::runtime::AgentProvider::Codex,
            thread_id: "thread".into(),
            native_session_id: None,
            native_turn_id: None,
            local_turn_id: None,
            rpc_request_id: None,
        },
        intent.invocation,
    )
    .await
    .unwrap();
    let second = resolve_retry_tx(
        &mut tx,
        &s.card,
        LEGACY_OWNER_USER_ID,
        "second",
        &KeepConversationInputRequest {
            carrier_id: s.carrier.clone(),
            submission_id: claimed.submission_id.as_str().into(),
            attempt_id: attempt.attempt_id.as_str().into(),
            revision: "0".into(),
        },
        &provenance(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_ne!(id(&first), id(&second));
    let carrier = current_input_carrier_tx(&mut tx, &s.card, &s.carrier)
        .await
        .unwrap();
    let next = retry_intent_tx(&mut tx, &carrier, &id(&second))
        .await
        .unwrap();
    assert_eq!(next.root_submission_id, s.root);
    assert_eq!(next.predecessor_submission_id, claimed.submission_id);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM conversation_submission_inputs WHERE retirement_reason='human_resolution'").fetch_one(&mut **tx).await.unwrap(),4);
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_retry_successor_interleaves_without_folding_accepted_inputs() {
    let s = seed().await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    let decision = resolve_retry_tx(
        &mut tx,
        &s.card,
        LEGACY_OWNER_USER_ID,
        "queue",
        &request(&s),
        &provenance(),
    )
    .await
    .unwrap()
    .unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &s.card, &s.carrier)
        .await
        .unwrap();
    let pending = carrier.checkpoint().pending[0].clone();
    accept_pending_human_tx(
        &mut tx,
        &mut carrier,
        LEGACY_OWNER_USER_ID,
        super::tests::body("ordinary tail"),
    )
    .await
    .unwrap();
    let tail = carrier.checkpoint().pending[1].clone();
    assert!(tail.queue_position() > pending.queue_position());
    let claimed = claim_initial_submission_tx(
        &mut tx,
        &mut carrier,
        InitialClaim::ExplicitSuccessor {
            retry_intent_id: id(&decision),
        },
        &provenance(),
    )
    .await
    .unwrap();
    assert_eq!(claimed.plan, s.render);
    assert_eq!(carrier.checkpoint().pending, vec![tail]);
    assert_eq!(
        carrier.checkpoint().active.as_ref().unwrap().submission_ids,
        vec![claimed.submission_id]
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_retry_inactive_text_plan_reopens_without_git_dependency() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("retry.db").display()
    );
    let repo = super::super::SqlxRepo::open(&url).await.unwrap();
    let (_, card) = super::tests::fixture_card(&repo).await;
    let s = super::retry_storage_tests::seed_repo(repo, card, false).await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    let decision = resolve_retry_tx(
        &mut tx,
        &s.card,
        LEGACY_OWNER_USER_ID,
        "reopen",
        &request(&s),
        &provenance(),
    )
    .await
    .unwrap()
    .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("UPDATE worker_sessions SET state='failed' WHERE id=?1")
        .bind(&s.carrier)
        .execute(s.repo.pool())
        .await
        .unwrap();
    let (card, carrier_id, root, expected) = (s.card, s.carrier, s.root, s.render);
    drop(s.repo);
    let repo = super::super::SqlxRepo::open(&url).await.unwrap();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    // This fixture has no frozen document heads. Remove its unrelated VCS graph
    // after boot, so the actual retry reader must hydrate using canonical facts only.
    sqlx::query(
        "DELETE FROM track_vcs_refs; DELETE FROM track_vcs_commits; DELETE FROM track_vcs_objects",
    )
    .execute(&mut **tx)
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM track_vcs_commits")
            .fetch_one(&mut **tx)
            .await
            .unwrap(),
        0,
        "retry text hydration must work with no git commits present"
    );
    let carrier = current_input_fact_carrier_tx(&mut tx, &card, &carrier_id)
        .await
        .unwrap();
    let intent = retry_intent_tx(&mut tx, &carrier, &id(&decision))
        .await
        .unwrap();
    assert!(!carrier.is_execution_current());
    assert_eq!(carrier.checkpoint().pending, vec![intent.pending]);
    assert_eq!(intent.root_submission_id, root);
    assert_eq!(intent.plan.plan, expected);
    assert_eq!(intent.invocation.model.as_deref(), Some("original-model"));
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_retry_racing_keys_has_one_effective_consumer() {
    let s = seed().await;
    let args = request(&s);
    let (card, worker) = (s.card.clone(), s.carrier.clone());
    let repo = std::sync::Arc::new(s.repo);
    let start = std::sync::Arc::new(tokio::sync::Barrier::new(3));
    let mut tasks = vec![];
    for key in ["race-a", "race-b"] {
        let (repo, start, card, args) = (repo.clone(), start.clone(), card.clone(), args.clone());
        tasks.push(tokio::spawn(async move {
            start.wait().await;
            let mut tx = begin_immediate_tx(repo.db()).await?;
            let result = resolve_retry_tx(
                &mut tx,
                &card,
                LEGACY_OWNER_USER_ID,
                key,
                &args,
                &provenance(),
            )
            .await;
            if result.is_ok() {
                tx.commit().await?;
            }
            result
        }));
    }
    start.wait().await;
    let mut winners = vec![];
    for task in tasks {
        if let Ok(Ok(decision)) = task.await.unwrap() {
            winners.push(decision);
        }
    }
    assert_eq!(winners.len(), 1);
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &card, &worker)
        .await
        .unwrap();
    assert_eq!(carrier.checkpoint().pending.len(), 1);
    assert_eq!(carrier.checkpoint().receipts.len(), 1);
    claim_initial_submission_tx(
        &mut tx,
        &mut carrier,
        InitialClaim::ExplicitSuccessor {
            retry_intent_id: id(&winners[0]),
        },
        &provenance(),
    )
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM conversation_submission_inputs WHERE effective=1"
        )
        .fetch_one(&mut **tx)
        .await
        .unwrap(),
        2
    );
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(DISTINCT submission_id) FROM conversation_submission_inputs WHERE effective=1").fetch_one(&mut **tx).await.unwrap(),1);
    tx.commit().await.unwrap();
}

mod availability;
