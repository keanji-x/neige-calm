//! Replay after an actual installation and queue ACK, using the same durable action.
use super::completed_commit_tests::Fixture;
use super::handoff_activation_test_support::{original, result, runtime, stored};
use super::handoff_cutover_tests::{commit, plan};
use super::*;
use crate::db::ServerRepoReadExt;
use crate::model::CardRole;
use crate::operation::OperationReceipt;
use crate::operation::handoff_activation::{self, ActivationResult};

#[tokio::test]
async fn handoff_activation_post_install_failure_preserves_ack_and_pending_suffix_on_retry() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let event = |key: &str| Event::TrackWakeRequested {
        track_id: a.harness.inner.track_id.clone(),
        source: "activation-replay".into(),
        key: crate::model::new_id(),
        text: key.into(),
    };
    let (floor, entry) = a.event_entry(&event("before-first-ack")).await;
    a.harness
        .observe_envelope(entry.observation(), floor)
        .await
        .unwrap();
    let plan = plan(&a, &b).await;
    let cutover = original(commit(&a, plan.clone()).await.unwrap());
    sqlx::query(concat!("CREATE TRIGGER fail_activated_checkpoint BEFORE UPDATE OF tx_output_json ON operations WHEN ","NEW.kind='handoff-activation' AND json_extract(NEW.tx_output_json,'$.result.state')='activated' ","BEGIN SELECT RAISE(ABORT,'post installation checkpoint failure'); END"))
        .execute(a.repo.pool()).await.unwrap();
    let (runtime, registry) = runtime(&a);
    let receipt = handoff_activation::activate(&runtime, cutover.clone())
        .await
        .unwrap();
    assert!(matches!(
        result(&receipt),
        ActivationResult::NeedsRecovery { .. }
    ));
    let installed = registry
        .get(&plan.scope.new_session_id)
        .expect("installation preceded the failed final checkpoint");
    tokio::time::timeout(Duration::from_secs(10), async {
        while a.daemon.turn_start_count_for_test() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("first input must reach the real issuance owner");
    let (suffix, entry) = b.event_entry(&event("after-first-ack")).await;
    installed
        .observe_envelope(entry.observation(), suffix)
        .await
        .unwrap();
    let acknowledged = stored(&a, &plan.scope.new_session_id).await;
    assert!(
        !acknowledged
            .pending_entries()
            .iter()
            .any(|entry| matches!(entry.observation(), Observation::SystemContext { .. }))
    );
    assert_eq!(acknowledged.push_watermark, suffix);
    sqlx::query("DROP TRIGGER fail_activated_checkpoint")
        .execute(a.repo.pool())
        .await
        .unwrap();
    let OperationReceipt::AwaitingRetry { attempt, .. } = receipt else {
        panic!()
    };
    assert!(matches!(
        result(
            &handoff_activation::retry(&runtime, cutover, attempt)
                .await
                .unwrap()
        ),
        ActivationResult::Activated { .. }
    ));
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 1);
    let snapshot = stored(&a, &plan.scope.new_session_id).await;
    assert!(snapshot.push_watermark >= suffix);
    let pending = snapshot
        .pending_entries()
        .iter()
        .map(|entry| entry.observation().to_turn_text())
        .collect::<Vec<_>>()
        .join("\n");
    let issued = serde_json::to_string(&a.daemon.started_turns_for_test()).unwrap();
    let all = format!("{issued}\n{pending}");
    assert_eq!(
        all.matches("Native provider history is not retained")
            .count(),
        1,
        "an ACKed first briefing cannot be reinserted from the older operation snapshot"
    );
    assert_eq!(all.matches("before-first-ack").count(), 1);
    assert!(
        all.contains("after-first-ack"),
        "pending suffix at or below its ACK watermark must survive installation replay"
    );
    registry
        .shutdown_and_remove(&plan.scope.new_session_id)
        .await
        .unwrap();
}

#[tokio::test]
async fn handoff_activation_ignored_snapshot_write_cannot_issue_without_durable_briefing() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let plan = plan(&a, &b).await;
    let cutover = original(commit(&a, plan.clone()).await.unwrap());
    sqlx::query(concat!(
        "CREATE TRIGGER ignore_prepared_snapshot BEFORE UPDATE OF handle_state_json ON worker_sessions ",
        "WHEN OLD.state='pending_activation' BEGIN SELECT RAISE(IGNORE); END"
    )).execute(a.repo.pool()).await.unwrap();
    let (runtime, registry) = runtime(&a);
    let receipt = handoff_activation::activate(&runtime, cutover.clone())
        .await
        .unwrap();
    assert!(matches!(
        receipt,
        OperationReceipt::Settled(crate::operation::OperationResult {
            outcome: crate::operation::OperationOutcome::Failed {
                from_phase: crate::operation::PhaseTag::Pending,
                last_error_class: Some(_),
                ..
            },
            ..
        })
    ));
    assert!(matches!(
        handoff_activation::query(&runtime, &cutover).await.unwrap(),
        ActivationResult::NeedsRecovery { .. }
    ));
    assert!(a.daemon.started_thread_params_for_test().is_empty());
    assert!(registry.get(&plan.scope.new_session_id).is_none());
    let row = a
        .repo
        .session_get_by_id(&plan.scope.new_session_id.as_str().into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.state,
        crate::session_projection_repo::WorkerSessionState::PendingActivation
    );
    assert!(row.thread_id.is_none() && row.mcp_token_hash.is_none());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM planner_handoff_activations")
            .fetch_one(a.repo.pool())
            .await
            .unwrap(),
        0
    );
}
