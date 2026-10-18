//! Real durable operation, native stub and run-loop installation entry points.
use super::completed_commit_tests::Fixture;
use super::handoff_activation_test_support::{
    original, result, runtime, stored, wait_for_resource_intent,
};
use super::handoff_cutover_tests::{commit, plan};
use super::*;
use crate::db::prelude::*;
use crate::model::CardRole;
use crate::operation::OperationReceipt;
use crate::operation::handoff_activation::{self, ActivationResult};

#[tokio::test]
async fn handoff_activation_exact_pending_installs_fresh_resources_and_replays_once() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let worker = a.sibling(CardRole::Worker).await;
    let worker_before = a
        .repo
        .session_get_by_id(&worker.harness.inner.worker_session_id.as_str().into())
        .await
        .unwrap()
        .unwrap();
    crate::operation::launch_cleanup_test_support::claimed_task(a.repo.clone(), a.harness.inner.events.clone(),
        crate::state::WriteContext::new(a.harness.inner.card_role_cache.clone(), a.harness.inner.track_area_cache.clone()),
        a.harness.inner.track_id.as_str(), serde_json::json!({"key":"unrelated","kind":"terminal","command":"true","ready":true,"declared_by":"user"})).await;
    let mut tx = crate::db::sqlite::begin_immediate_tx(a.repo.pool())
        .await
        .unwrap();
    let (scope, event) = crate::ask::ask_requested_tx(
        &mut tx,
        &a.harness.inner.card_id,
        vec![crate::event::AskQuestion {
            title: "Existing user decision".into(),
            options: vec!["Keep".into()],
        }],
    )
    .await
    .unwrap();
    let ask_id = crate::db::sqlite::append_decision_event_in_tx(
        &mut tx,
        &ActorId::for_agent_session(
            CardRole::Planner,
            &crate::session_projection_repo::AgentProvider::Codex,
            a.harness.inner.worker_session_id.as_str().into(),
        ),
        &scope,
        None,
        &event,
    )
    .await
    .unwrap();
    for f in [&a, &b] {
        crate::mcp_server::wiring::mint_and_persist_card_token(
            &mut tx,
            f.harness.inner.card_id.as_str(),
            &f.harness.inner.worker_session_id,
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    let original_ask = crate::ask::stored_ask(a.repo.as_ref(), &a.harness.inner.track_id, ask_id)
        .await
        .unwrap();
    let old_hashes: Vec<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id IN (?1,?2)")
            .bind(&a.harness.inner.worker_session_id)
            .bind(&b.harness.inner.worker_session_id)
            .fetch_all(a.repo.pool())
            .await
            .unwrap();
    let plan = plan(&a, &b).await;
    let cutover = original(commit(&a, plan.clone()).await.unwrap());
    let before = super::handoff_cutover_tests::data_image(a.repo.pool())
        .await
        .into_iter()
        .filter(|(name, _)| {
            name.contains("task") || name.contains("ask") || name.contains("approval")
        })
        .collect::<Vec<_>>();
    let (runtime, registry) = runtime(&a);
    let receipt = handoff_activation::activate(&runtime, cutover.clone())
        .await
        .unwrap();
    assert_eq!(
        result(&receipt),
        ActivationResult::Activated {
            session_id: plan.scope.new_session_id.clone()
        }
    );
    assert!(registry.get(&plan.scope.new_session_id).is_some());
    let row = a
        .repo
        .session_get_by_id(&plan.scope.new_session_id.as_str().into())
        .await
        .unwrap()
        .unwrap();
    assert!(row.state.is_active_authority());
    assert!(row.mcp_token_hash.is_some());
    assert!(!old_hashes.contains(row.mcp_token_hash.as_ref().unwrap()));
    assert_ne!(row.thread_id.as_deref(), Some("thread-commit-wake"));
    assert_eq!(
        result(
            &handoff_activation::activate(&runtime, cutover)
                .await
                .unwrap()
        ),
        result(&receipt)
    );
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 1);
    for old in [&plan.scope.source_session_id, &plan.scope.target_session_id] {
        let row = a
            .repo
            .session_get_by_id(&old.as_str().into())
            .await
            .unwrap()
            .unwrap();
        assert!(!row.state.is_active_authority());
        assert!(row.mcp_token_hash.is_none());
    }
    let after = super::handoff_cutover_tests::data_image(a.repo.pool())
        .await
        .into_iter()
        .filter(|(name, _)| {
            name.contains("task") || name.contains("ask") || name.contains("approval")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        after, before,
        "activation must not rewrite task, attempt, claim or user decision identities"
    );
    let retained_ask = crate::ask::stored_ask(a.repo.as_ref(), &a.harness.inner.track_id, ask_id)
        .await
        .unwrap();
    assert_eq!(retained_ask.actor, original_ask.actor);
    assert_eq!(retained_ask.questions, original_ask.questions);
    assert_eq!(retained_ask.delivery, original_ask.delivery);
    let worker_after = a
        .repo
        .session_get_by_id(&worker.harness.inner.worker_session_id.as_str().into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(worker_after).unwrap(),
        serde_json::to_value(worker_before).unwrap()
    );
    registry
        .shutdown_and_remove(&plan.scope.new_session_id)
        .await
        .unwrap();
}

#[tokio::test]
async fn handoff_activation_authhold_blocks_and_requires_explicit_original_retry() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let plan = plan(&a, &b).await;
    let cutover = original(commit(&a, plan.clone()).await.unwrap());
    a.daemon
        .emit_notification_for_test(crate::codex_appserver::Notification::Other {
            method: "error".into(),
            params: serde_json::json!({"error":{"codexErrorInfo":"unauthorized"}}),
        });
    assert!(a.daemon.authentication_hold().is_some());
    let (runtime, registry) = runtime(&a);
    let receipt = handoff_activation::activate(&runtime, cutover.clone())
        .await
        .unwrap();
    assert!(matches!(result(&receipt), ActivationResult::Blocked { .. }));
    assert!(
        sqlx::query("UPDATE worker_sessions SET state='starting' WHERE id=?1")
            .bind(&plan.scope.new_session_id)
            .execute(a.repo.pool())
            .await
            .is_err(),
        "an activation claim alone must not authorize the SQL transition"
    );
    assert!(registry.get(&plan.scope.new_session_id).is_none());
    assert!(a.daemon.started_thread_params_for_test().is_empty());
    let snapshot = stored(&a, &plan.scope.new_session_id).await;
    assert!(
        snapshot
            .pending_entries()
            .iter()
            .any(|entry| matches!(entry.observation(), Observation::SystemContext { .. }))
    );
    a.daemon
        .emit_notification_for_test(crate::codex_appserver::Notification::Other {
            method: "account/login/completed".into(),
            params: serde_json::json!({"success":true,"error":null}),
        });
    assert!(a.daemon.authentication_hold().is_none());
    runtime.drive().await.unwrap();
    assert!(
        a.daemon.started_thread_params_for_test().is_empty(),
        "hold release must not auto-rearm"
    );
    let OperationReceipt::AwaitingRetry { attempt, .. } = receipt else {
        panic!()
    };
    let done = handoff_activation::retry(&runtime, cutover, attempt)
        .await
        .unwrap();
    assert!(matches!(result(&done), ActivationResult::Activated { .. }));
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 1);
    registry
        .shutdown_and_remove(&plan.scope.new_session_id)
        .await
        .unwrap();
}

#[tokio::test]
async fn handoff_activation_unknown_resource_never_blindly_reissues() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let plan = plan(&a, &b).await;
    let cutover = original(commit(&a, plan.clone()).await.unwrap());
    a.daemon.fail_next_thread_start_for_test();
    let (runtime, registry) = runtime(&a);
    let receipt = handoff_activation::activate(&runtime, cutover.clone())
        .await
        .unwrap();
    assert!(matches!(result(&receipt), ActivationResult::Unknown { .. }));
    assert_eq!(
        handoff_activation::query(&runtime, &cutover).await.unwrap(),
        result(&receipt)
    );
    let OperationReceipt::AwaitingRetry { attempt, .. } = receipt else {
        panic!()
    };
    let retried = handoff_activation::retry(&runtime, cutover, attempt)
        .await
        .unwrap();
    assert!(matches!(result(&retried), ActivationResult::Unknown { .. }));
    assert_eq!(
        a.daemon.started_thread_params_for_test().len(),
        1,
        "only the original request may reach the resource owner"
    );
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
    assert!(row.mcp_token_hash.is_none() && row.thread_id.is_none());
}

#[tokio::test]
async fn handoff_activation_queue_and_briefing_are_durable_before_any_resource_request() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    super::handoff_pause_tests::system(&a, 4).await;
    super::handoff_pause_tests::system(&a, 9).await;
    for f in [&a, &b] {
        f.repo
            .harness_item_insert(
                &f.harness.inner.worker_session_id,
                f.harness.inner.card_id.as_str(),
                f.harness.inner.track_id.as_str(),
                "thread-commit-wake",
                Some("historical-turn"),
                Some("excerpt"),
                Some("agentMessage"),
                "item/completed",
                &serde_json::json!({"text":"界".repeat(15000)}).to_string(),
                None,
            )
            .await
            .unwrap();
    }
    let plan = plan(&a, &b).await;
    let cutover = original(commit(&a, plan.clone()).await.unwrap());
    let baseline = stored(&a, &plan.scope.new_session_id).await;
    a.daemon.fail_next_thread_start_for_test();
    let (runtime, _) = runtime(&a);
    let receipt = handoff_activation::activate(&runtime, cutover.clone())
        .await
        .unwrap();
    assert!(matches!(result(&receipt), ActivationResult::Unknown { .. }));
    let snapshot = stored(&a, &plan.scope.new_session_id).await;
    assert_eq!(snapshot.push_watermark, baseline.push_watermark);
    let entries = snapshot.pending_entries();
    assert_eq!(&entries[1..], baseline.pending_entries());
    let Observation::SystemContext { text } = entries[0].observation() else {
        panic!()
    };
    assert!(text.contains("Native provider history is not retained"));
    assert!(
        text.contains(&plan.scope.source_session_id)
            && text.contains(&plan.scope.target_session_id)
    );
    assert!(text.chars().count() <= 32768);
    assert!(text.contains("truncated at Unicode character boundary"));
    assert!(text.matches('界').count() <= 12000);
    let OperationReceipt::AwaitingRetry { attempt, .. } = receipt else {
        panic!()
    };
    handoff_activation::retry(&runtime, cutover, attempt)
        .await
        .unwrap();
    assert_eq!(stored(&a, &plan.scope.new_session_id).await, snapshot);
}

#[tokio::test]
async fn handoff_activation_rejects_wrong_cutover_closed_track_and_changed_configuration() {
    for defect in ["wrong", "closed", "config", "root_missing", "root"] {
        let a = Fixture::new().await;
        let b = a.sibling(CardRole::Assistant).await;
        let plan = plan(&a, &b).await;
        let mut cutover = original(commit(&a, plan.clone()).await.unwrap());
        match defect {
            "wrong" => cutover = "not-a-cutover".into(),
            "closed" => {
                sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1")
                    .bind(plan.scope.track_id.as_str())
                    .execute(a.repo.pool())
                    .await
                    .unwrap();
            }
            "config" => {
                sqlx::query(
                    "UPDATE cards SET payload=json_set(payload,'$.model','changed') WHERE id=?1",
                )
                .bind(plan.scope.target_card_id.as_str())
                .execute(a.repo.pool())
                .await
                .unwrap();
            }
            "root_missing" => {
                sqlx::query("UPDATE tracks SET root_session_id=NULL WHERE id=?1")
                    .bind(plan.scope.track_id.as_str())
                    .execute(a.repo.pool())
                    .await
                    .unwrap();
            }
            "root" => {
                sqlx::query("UPDATE tracks SET root_session_id=?1 WHERE id=?2")
                    .bind(&plan.scope.source_session_id)
                    .bind(plan.scope.track_id.as_str())
                    .execute(a.repo.pool())
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let (runtime, _) = runtime(&a);
        assert!(
            handoff_activation::activate(&runtime, cutover)
                .await
                .is_err(),
            "{defect}"
        );
        assert!(a.daemon.started_thread_params_for_test().is_empty());
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
    }
}

#[tokio::test]
async fn handoff_activation_resource_checkpoint_failure_is_unknown_and_never_remints() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let plan = plan(&a, &b).await;
    let cutover = original(commit(&a, plan.clone()).await.unwrap());
    sqlx::query(concat!("CREATE TRIGGER fail_activation_binding BEFORE UPDATE OF state ON worker_sessions WHEN ","OLD.state='pending_activation' BEGIN SELECT RAISE(ABORT,'activation binding rollback'); END"))
        .execute(a.repo.pool()).await.unwrap();
    let (runtime, _) = runtime(&a);
    let receipt = handoff_activation::activate(&runtime, cutover.clone())
        .await
        .unwrap();
    assert!(matches!(result(&receipt), ActivationResult::Unknown { .. }));
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 1);
    sqlx::query("DROP TRIGGER fail_activation_binding")
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
        ActivationResult::Unknown { .. }
    ));
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 1);
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
    assert!(row.mcp_token_hash.is_none());
}

#[tokio::test]
async fn handoff_activation_proven_resources_recover_without_repeating_effect_or_briefing() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let plan = plan(&a, &b).await;
    let cutover = original(commit(&a, plan.clone()).await.unwrap());
    sqlx::query(concat!("CREATE TRIGGER fail_activation_install BEFORE UPDATE OF handle_state_json ON worker_sessions WHEN ","OLD.state='starting' BEGIN SELECT RAISE(ABORT,'activation installation checkpoint'); END"))
        .execute(a.repo.pool()).await.unwrap();
    let (runtime, registry) = runtime(&a);
    let receipt = handoff_activation::activate(&runtime, cutover.clone())
        .await
        .unwrap();
    assert!(matches!(
        result(&receipt),
        ActivationResult::NeedsRecovery { .. }
    ));
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 1);
    assert!(registry.get(&plan.scope.new_session_id).is_none());
    sqlx::query("DROP TRIGGER fail_activation_install")
        .execute(a.repo.pool())
        .await
        .unwrap();
    let OperationReceipt::AwaitingRetry { attempt, .. } = receipt else {
        panic!()
    };
    let receipt = handoff_activation::retry(&runtime, cutover, attempt)
        .await
        .unwrap();
    assert!(matches!(
        result(&receipt),
        ActivationResult::Activated { .. }
    ));
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 1);
    registry
        .shutdown_and_remove(&plan.scope.new_session_id)
        .await
        .unwrap();
}

#[tokio::test]
async fn handoff_activation_original_unknown_survives_lost_reply_and_actual_file_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        temp.path().join("activation.sqlite").display()
    );
    let cutover;
    let session;
    {
        let a = Fixture::for_role_at(CardRole::Planner, &url).await;
        let b = a.sibling(CardRole::Assistant).await;
        let plan = plan(&a, &b).await;
        session = plan.scope.new_session_id.clone();
        cutover = original(commit(&a, plan).await.unwrap());
        a.daemon.fail_next_thread_start_for_test();
        let (runtime, _) = runtime(&a);
        let (reply, receiver) = tokio::sync::oneshot::channel();
        drop(receiver);
        let receipt = handoff_activation::activate(&runtime, cutover.clone())
            .await
            .unwrap();
        assert!(matches!(result(&receipt), ActivationResult::Unknown { .. }));
        assert!(reply.send(receipt).is_err());
    }
    // Reopen the same file without fabricating the native provider's lost resource result.
    let repo = Arc::new(crate::db::sqlite::SqlxRepo::open(&url).await.unwrap());
    let operations = crate::operation::SqlxOperationRepo::new(repo.pool().clone());
    use crate::operation::OperationRepo;
    let key = crate::operation::OperationKey {
        operation_key: format!("legacy-cutover:{cutover}"),
        idempotency_key: Some(format!("legacy-cutover:{cutover}")),
        payload_hash: String::new(),
    };
    let op = operations
        .find_by_idempotency_key(
            crate::operation::handoff_coordinator::HANDOFF_COORDINATOR,
            &key,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        op.phase,
        crate::operation::Phase::AwaitingRetry { .. }
    ));
    assert!(matches!(
        serde_json::from_value::<crate::operation::handoff_coordinator::Progress>(
            op.tx_output.unwrap().result
        )
        .unwrap(),
        crate::operation::handoff_coordinator::Progress::Committed {
            resources: crate::operation::handoff_coordinator::Resources::Unknown { .. },
            ..
        }
    ));
    let row = repo
        .session_get_by_id(&session.as_str().into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.state,
        crate::session_projection_repo::WorkerSessionState::PendingActivation
    );
    let snapshot = HarnessSnapshot::parse_known(row.handle_state_json.unwrap()).unwrap();
    assert_eq!(
        snapshot
            .pending_entries()
            .iter()
            .filter(|entry| matches!(entry.observation(), Observation::SystemContext { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn handoff_activation_retired_target_never_regains_authority_after_successor_failure() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let plan = plan(&a, &b).await;
    let cutover = original(commit(&a, plan.clone()).await.unwrap());
    let (runtime, registry) = runtime(&a);
    assert!(matches!(
        result(
            &handoff_activation::activate(&runtime, cutover)
                .await
                .unwrap()
        ),
        ActivationResult::Activated { .. }
    ));
    registry
        .shutdown_and_remove(&plan.scope.new_session_id)
        .await
        .unwrap();
    // Remove the active-index guard, isolating the retirement authority invariant.
    sqlx::query("UPDATE worker_sessions SET state='failed',mcp_token_hash=NULL WHERE id=?1")
        .bind(&plan.scope.new_session_id)
        .execute(a.repo.pool())
        .await
        .unwrap();
    for retired in [&plan.scope.source_session_id, &plan.scope.target_session_id] {
        assert!(
            sqlx::query("UPDATE worker_sessions SET state='idle' WHERE id=?1")
                .bind(retired)
                .execute(a.repo.pool())
                .await
                .is_err()
        );
        assert!(
            sqlx::query("UPDATE worker_sessions SET mcp_token_hash='old-token' WHERE id=?1")
                .bind(retired)
                .execute(a.repo.pool())
                .await
                .is_err()
        );
        assert!(
            sqlx::query("UPDATE cards SET session_id=?1 WHERE id=?2")
                .bind(retired)
                .bind(plan.scope.target_card_id.as_str())
                .execute(a.repo.pool())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn handoff_activation_blocked_claim_preserves_parent_track_deletion_contract() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let plan = plan(&a, &b).await;
    let cutover = original(commit(&a, plan.clone()).await.unwrap());
    a.daemon
        .emit_notification_for_test(crate::codex_appserver::Notification::Other {
            method: "error".into(),
            params: serde_json::json!({"error":{"codexErrorInfo":"unauthorized"}}),
        });
    let (runtime, _) = runtime(&a);
    assert!(matches!(
        result(
            &handoff_activation::activate(&runtime, cutover)
                .await
                .unwrap()
        ),
        ActivationResult::Blocked { .. }
    ));
    assert!(
        a.repo
            .track_delete(plan.scope.track_id.as_str())
            .await
            .is_err()
    );
    assert!(
        a.repo
            .track_get(plan.scope.track_id.as_str())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn handoff_activation_two_explicit_callers_share_one_persistent_operation() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let plan = plan(&a, &b).await;
    let cutover = original(commit(&a, plan.clone()).await.unwrap());
    let (runtime, registry) = runtime(&a);
    let runtime = Arc::new(runtime);
    let native = a.daemon.lock_thread_start_serial_for_test().await;
    let first = tokio::spawn({
        let runtime = runtime.clone();
        let cutover = cutover.clone();
        async move { handoff_activation::activate(&runtime, cutover).await }
    });
    wait_for_resource_intent(&a).await;
    let owner: String = sqlx::query_scalar(
        "SELECT owner_operation_id FROM planner_handoff_adoptions WHERE legacy_operation_id=?1",
    )
    .bind(&cutover)
    .fetch_one(a.repo.pool())
    .await
    .unwrap();
    let early = tokio::time::timeout(
        Duration::from_secs(10),
        handoff_activation::activate(&runtime, cutover.clone()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(early,OperationReceipt::Running { operation_id,attempt:1 } if operation_id==owner)
    );
    assert!(a.daemon.started_thread_params_for_test().is_empty());
    drop(native);
    let completed = first.await.unwrap().unwrap();
    assert!(
        matches!(&completed,OperationReceipt::Settled(crate::operation::OperationResult { op_id,.. }) if op_id==&owner)
    );
    assert_eq!(
        result(&completed),
        ActivationResult::Activated {
            session_id: plan.scope.new_session_id.clone()
        }
    );
    let installed = registry.get(&plan.scope.new_session_id).unwrap();
    let replay = handoff_activation::activate(&runtime, cutover)
        .await
        .unwrap();
    assert!(
        matches!(&replay,OperationReceipt::Settled(crate::operation::OperationResult { op_id,.. }) if op_id==&owner)
    );
    assert_eq!(result(&replay), result(&completed));
    assert!(Arc::ptr_eq(
        &installed.inner,
        &registry.get(&plan.scope.new_session_id).unwrap().inner
    ));
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 1);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM operations WHERE kind='handoff-workflow'")
            .fetch_one(a.repo.pool())
            .await
            .unwrap();
    assert_eq!(count, 1);
    registry
        .shutdown_and_remove(&plan.scope.new_session_id)
        .await
        .unwrap();
}

#[tokio::test]
async fn handoff_activation_cancellation_startup_replay_keeps_unknown_intent_without_native_repeat()
{
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let plan = plan(&a, &b).await;
    let cutover = original(commit(&a, plan.clone()).await.unwrap());
    let (runtime, _) = runtime(&a);
    let runtime = Arc::new(runtime);
    let native = a.daemon.lock_thread_start_serial_for_test().await;
    let task = tokio::spawn({
        let runtime = runtime.clone();
        let cutover = cutover.clone();
        async move { handoff_activation::activate(&runtime, cutover).await }
    });
    wait_for_resource_intent(&a).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    drop(native);
    let recovery = runtime.recover_on_boot().await.unwrap();
    runtime.apply_recovery(recovery).await.unwrap();
    let receipt = handoff_activation::query(&runtime, &cutover).await.unwrap();
    assert!(matches!(receipt, ActivationResult::Unknown { .. }));
    assert!(a.daemon.started_thread_params_for_test().is_empty());
    assert_eq!(
        stored(&a, &plan.scope.new_session_id)
            .await
            .pending_entries()
            .iter()
            .filter(|entry| matches!(entry.observation(), Observation::SystemContext { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn handoff_activation_suffix_and_briefing_reach_first_input_once_without_losing_floor_queue()
{
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let event = |key: &str| Event::TrackWakeRequested {
        track_id: a.harness.inner.track_id.clone(),
        source: "activation-fixture".into(),
        key: key.into(),
        text: key.into(),
    };
    let (floor, original_entry) = a.event_entry(&event("queued-prefix")).await;
    a.harness
        .observe_envelope(original_entry.observation(), floor)
        .await
        .unwrap();
    sqlx::query(concat!("UPDATE cards SET payload=json_set(payload,'$.model','activation-model','$.model_ever_set',","json('true'),'$.reasoning_effort','high','$.reasoning_effort_ever_set',json('true')) WHERE id=?1"))
        .bind(b.harness.inner.card_id.as_str()).execute(a.repo.pool()).await.unwrap();
    let context = crate::template_context::TemplateContext::new(
        "Saved method".into(),
        "activation-original-working-method".into(),
    );
    sqlx::query(
        "UPDATE cards SET payload=json_set(payload,'$.template_context',json(?1)) WHERE id=?2",
    )
    .bind(serde_json::to_string(&context).unwrap())
    .bind(a.harness.inner.card_id.as_str())
    .execute(a.repo.pool())
    .await
    .unwrap();
    let plan = plan(&a, &b).await;
    let cutover = original(commit(&a, plan.clone()).await.unwrap());
    let (runtime, registry) = runtime(&a);
    let runtime = Arc::new(runtime);
    let native = a.daemon.lock_thread_start_serial_for_test().await;
    let task = tokio::spawn({
        let runtime = runtime.clone();
        let cutover = cutover.clone();
        async move { handoff_activation::activate(&runtime, cutover).await }
    });
    wait_for_resource_intent(&a).await;
    let snapshot = stored(&a, &plan.scope.new_session_id).await;
    assert_eq!(snapshot.push_watermark, floor);
    assert_eq!(&snapshot.pending_entries()[1..], &[original_entry]);
    let (suffix, _) = b.event_entry(&event("activation-suffix")).await;
    drop(native);
    assert!(matches!(
        result(&task.await.unwrap().unwrap()),
        ActivationResult::Activated { .. }
    ));
    tokio::time::timeout(Duration::from_secs(10), async {
        while a.daemon.turn_start_count_for_test() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("installed harness must issue the real initial input");
    let selections = a.daemon.started_turn_selections_for_test();
    assert_eq!(selections[0].1.model.as_deref(), Some("activation-model"));
    assert_eq!(selections[0].1.effort.as_deref(), Some("high"));
    assert_eq!(
        a.daemon.started_turn_approvals_for_test(),
        vec![crate::codex_appserver::TurnApprovals::Explicit(
            crate::planner_permission_mode::PlannerPermissionMode::Never
        )]
    );
    assert!(
        a.daemon.started_thread_params_for_test()[0]
            .0
            .as_ref()
            .unwrap()
            .contains("activation-original-working-method")
    );
    let input = serde_json::to_string(&a.daemon.started_turns_for_test()).unwrap();
    assert!(input.contains("queued-prefix") && input.contains("activation-suffix"));
    assert_eq!(
        input
            .matches("Native provider history is not retained")
            .count(),
        1
    );
    assert!(stored(&a, &plan.scope.new_session_id).await.push_watermark >= suffix);
    assert!(matches!(
        result(
            &handoff_activation::activate(&runtime, cutover)
                .await
                .unwrap()
        ),
        ActivationResult::Activated { .. }
    ));
    let input = serde_json::to_string(&a.daemon.started_turns_for_test()).unwrap();
    assert_eq!(
        input
            .matches("Native provider history is not retained")
            .count(),
        1
    );
    registry
        .shutdown_and_remove(&plan.scope.new_session_id)
        .await
        .unwrap();
}
