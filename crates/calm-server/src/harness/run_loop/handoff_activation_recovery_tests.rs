//! Replay after an actual installation and queue ACK, using the same durable action.
use super::completed_commit_tests::Fixture;
use super::handoff_activation_test_support::{original, result, runtime, stored};
use super::handoff_cutover_tests::{commit, plan};
use super::*;
use crate::db::ServerRepoReadExt;
use crate::model::CardRole;
use crate::operation::OperationReceipt;
use crate::operation::handoff_activation::{self, ActivationResult};
use crate::session_projection_repo::WorkerSessionProjectionRepo;

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

async fn notification(a: &Fixture, owner: &PlannerHarness, method: &str, params: Value) {
    let hook = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "harness-notification-completed",
        owner.inner.card_id.as_str(),
        hook.clone(),
    );
    a.daemon
        .emit_notification_for_test(crate::codex_appserver::Notification::parse(
            method.into(),
            params.clone(),
        ));
    tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
        .await
        .unwrap();
    hook.release.notify_one();
    // The shared hook can also observe an earlier queued notification. Wait for this
    // notification's public persisted fact, not merely whichever frame reached it first.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            owner.persist_snapshot().await.unwrap();
            let snapshot = owner.snapshot().await;
            let ready = match method {
                "thread/tokenUsage/updated" => snapshot.token_usage.as_ref().is_some_and(|usage| {
                    Some(usage.total_tokens)
                        == params["tokenUsage"]["total"]["totalTokens"].as_i64()
                }),
                "turn/completed" => {
                    snapshot.phase == HarnessPhaseTag::TurnCompleted
                        && snapshot.last_turn_id.as_deref() == params["turn"]["id"].as_str()
                }
                "item/completed" => owner
                    .inner
                    .repo
                    .harness_item_list_by_card(owner.inner.card_id.as_str(), 0, 500, false)
                    .await
                    .unwrap()
                    .iter()
                    .any(|row| row.item_uuid.as_deref() == params["item"]["id"].as_str()),
                _ => panic!("notification barrier requires an explicit persisted fact"),
            };
            if ready {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn running_snapshot(owner: &PlannerHarness, previous: Option<&str>) -> HarnessSnapshot {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            // persist_snapshot serializes with the notification/issuance owner, giving a
            // stable committed barrier rather than a turn/start counter or a timed sleep.
            owner.persist_snapshot().await.unwrap();
            let snapshot = owner.snapshot().await;
            if snapshot.phase == HarnessPhaseTag::TurnRunning
                && snapshot.last_turn_id.as_deref() != previous
            {
                return snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

async fn fail_final_checkpoint(a: &Fixture) {
    sqlx::query(concat!("CREATE TRIGGER fail_activated_checkpoint BEFORE UPDATE OF tx_output_json ON operations WHEN ",
        "NEW.kind='handoff-activation' AND json_extract(NEW.tx_output_json,'$.result.state')='activated' ",
        "BEGIN SELECT RAISE(ABORT,'post installation checkpoint failure'); END"))
        .execute(a.repo.pool()).await.unwrap();
}

async fn allow_final_checkpoint(a: &Fixture) {
    sqlx::query("DROP TRIGGER fail_activated_checkpoint")
        .execute(a.repo.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn handoff_activation_post_install_retry_preserves_completed_usage_and_replace_rewind() {
    // File-backed storage is reopened before retry; the runtime reads the same persisted
    // operation and session, with the original live registry owner still present.
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("activation.db").display()
    );
    let a = Fixture::for_role_at(CardRole::Planner, &url).await;
    let b = a.sibling(CardRole::Assistant).await;
    let plan = plan(&a, &b).await;
    let cutover = original(commit(&a, plan.clone()).await.unwrap());
    fail_final_checkpoint(&a).await;
    let (runtime, registry) = runtime(&a);
    let receipt = handoff_activation::activate(&runtime, cutover.clone())
        .await
        .unwrap();
    assert!(matches!(
        result(&receipt),
        ActivationResult::NeedsRecovery { .. }
    ));
    let owner = registry.get(&plan.scope.new_session_id).unwrap();
    let first = running_snapshot(&owner, None).await;
    let thread = first.last_thread_id.clone().unwrap();
    let first_turn = first.last_turn_id.clone().unwrap();
    a.daemon.clear_active_turn_for_test(&thread);
    notification(
        &a,
        &owner,
        "turn/completed",
        serde_json::json!({"threadId":thread,"turn":{"id":first_turn,"status":"completed"}}),
    )
    .await;
    let ack = owner
        .observe_user_message_durable(
            "editable second turn".into(),
            vec![],
            crate::harness::SendKey::unique_for_test(),
        )
        .await
        .unwrap();
    let second = running_snapshot(&owner, Some(&first_turn)).await;
    let second_turn = second.last_turn_id.unwrap();
    let client = ack.entry_id.unwrap();
    notification(&a, &owner, "item/completed", serde_json::json!({"threadId":thread,"turnId":second_turn,"item":{
        "id":"second-user-echo","clientId":client.as_str(),"type":"userMessage","content":[{"type":"text","text":"editable second turn"}]}})).await;
    notification(&a, &owner, "thread/tokenUsage/updated", serde_json::json!({"threadId":thread,"turnId":second_turn,
        "tokenUsage":{"last":{"totalTokens":1500},"total":{"totalTokens":3000},"modelContextWindow":200000}})).await;
    a.daemon.clear_active_turn_for_test(&thread);
    notification(
        &a,
        &owner,
        "turn/completed",
        serde_json::json!({"threadId":thread,"turn":{"id":second_turn,"status":"completed"}}),
    )
    .await;
    let replaced = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "harness-replace-completed",
        owner.inner.card_id.as_str(),
        replaced.clone(),
    );
    owner
        .replace_turn_durable(
            second_turn.clone(),
            "replacement after retry".into(),
            vec![],
            crate::harness::SendKey::unique_for_test(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), replaced.entered.notified())
        .await
        .unwrap();
    let before = stored(&a, &plan.scope.new_session_id).await;
    assert!(before.pending_rewind.is_some());
    assert_eq!(before.last_turn_id.as_deref(), Some(first_turn.as_str()));
    assert_eq!(before.token_usage.as_ref().unwrap().total_tokens, 3000);
    allow_final_checkpoint(&a).await;
    let reopened = crate::db::sqlite::SqlxRepo::open(&url).await.unwrap();
    assert_eq!(
        HarnessSnapshot::parse_known(
            reopened
                .session_projection_handle_state_by_id(&plan.scope.new_session_id)
                .await
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        before
    );
    drop(runtime);
    let runtime = super::handoff_activation_test_support::runtime_with_repo(
        &a,
        Arc::new(reopened),
        registry.clone(),
    );
    let OperationReceipt::AwaitingRetry { attempt, .. } = receipt else {
        panic!()
    };
    let issued = crate::shared_codex_appserver::TurnStartReturnHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    a.daemon
        .install_turn_start_return_hook_for_test(issued.clone());
    let retry = handoff_activation::retry(&runtime, cutover, attempt)
        .await
        .unwrap();
    assert!(matches!(result(&retry), ActivationResult::Activated { .. }));
    tokio::time::timeout(Duration::from_secs(10), issued.entered.notified())
        .await
        .unwrap();
    // thread/revert records how many inputs existed BEFORE the revert, so this pins order.
    let reverts = a.daemon.reverted_threads_for_test();
    issued.release.notify_one();
    assert_eq!(
        reverts,
        vec![(thread, second_turn, 2)],
        "durable rewind must precede replacement input"
    );
    let recovered = registry.get(&plan.scope.new_session_id).unwrap();
    let after = running_snapshot(&recovered, Some(&first_turn)).await;
    assert_eq!(after.token_usage, before.token_usage);
    assert_eq!(
        after.context_usage_invalidated,
        before.context_usage_invalidated
    );
    assert_eq!(
        after.last_report_body_sha256,
        before.last_report_body_sha256
    );
    assert!(after.push_watermark >= before.push_watermark);
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 1);
    let inputs = serde_json::to_string(&a.daemon.started_turns_for_test()).unwrap();
    assert_eq!(
        inputs
            .matches("Native provider history is not retained")
            .count(),
        1
    );
    assert_eq!(inputs.matches("replacement after retry").count(), 1);
    registry
        .shutdown_and_remove(&plan.scope.new_session_id)
        .await
        .unwrap();
}

#[tokio::test]
async fn handoff_activation_post_install_retry_running_interrupt_failure_restores_resumed() {
    for interrupt_fails in [false, true] {
        let a = Fixture::new().await;
        let b = a.sibling(CardRole::Assistant).await;
        let plan = plan(&a, &b).await;
        let cutover = original(commit(&a, plan.clone()).await.unwrap());
        fail_final_checkpoint(&a).await;
        let (runtime, registry) = runtime(&a);
        let receipt = handoff_activation::activate(&runtime, cutover.clone())
            .await
            .unwrap();
        let owner = registry.get(&plan.scope.new_session_id).unwrap();
        let before = running_snapshot(&owner, None).await;
        let turn = before.last_turn_id.clone().unwrap();
        notification(
            &a,
            &owner,
            "thread/tokenUsage/updated",
            serde_json::json!({"threadId":before.last_thread_id,"turnId":turn,
            "tokenUsage":{"last":{"totalTokens":1500},"total":{"totalTokens":3000}}}),
        )
        .await;
        owner
            .observe_user_message_durable(
                "must wait for recovery".into(),
                vec![],
                crate::harness::SendKey::unique_for_test(),
            )
            .await
            .unwrap();
        let before = stored(&a, &plan.scope.new_session_id).await;
        a.daemon.fail_turn_interrupt_for_test(interrupt_fails);
        allow_final_checkpoint(&a).await;
        let OperationReceipt::AwaitingRetry { attempt, .. } = receipt else {
            panic!()
        };
        let done = handoff_activation::retry(&runtime, cutover, attempt)
            .await
            .unwrap();
        assert!(matches!(result(&done), ActivationResult::Activated { .. }));
        let recovered = registry.get(&plan.scope.new_session_id).unwrap();
        let after = recovered.snapshot().await;
        assert_eq!(
            after.phase,
            HarnessPhaseTag::Resumed,
            "local shutdown is not a native terminal receipt"
        );
        assert_eq!(after.last_turn_id, before.last_turn_id);
        assert_eq!(after.token_usage, before.token_usage);
        assert_eq!(after.pending_entries(), before.pending_entries());
        assert_eq!(after.projection_client_id, before.projection_client_id);
        assert_eq!(after.turn_budget, before.turn_budget);
        let mut expected = before.clone();
        expected.phase = HarnessPhaseTag::Resumed;
        assert_eq!(
            after, expected,
            "the entire durable running checkpoint must survive recovery"
        );
        assert_eq!(a.daemon.turn_start_count_for_test(), 1);
        a.daemon.fail_turn_interrupt_for_test(false);
        registry
            .shutdown_and_remove(&plan.scope.new_session_id)
            .await
            .unwrap();
    }
}
