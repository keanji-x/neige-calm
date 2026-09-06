use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn task_recovery_claude_withdrawn_during_preparation_never_launches() {
    for (withdrawn, takeover) in [(false, false), (true, false), (false, true)] {
        let harness = claude_worker_harness().await;
        let write = WriteContext::new(
            harness.adapter.card_role_cache.clone(),
            harness.adapter.track_area_cache.clone(),
        );
        let fixture = crate::task_recovery::launch_test_support::recovered_claimed_task(harness.repo.clone(), harness.events.clone(), write,
        &harness.track_id, json!({"key":"launch", "kind":"claude", "goal":"bounded launch fixture", "ready":true, "declared_by":"user", "no_gate_reason":"launch admission test"})).await;
        let socket_dir = tempfile::tempdir().unwrap();
        let server = McpServer::new_for_test(crate::mcp_server::McpShimConfig {
            shim_bin: socket_dir.path().join("shim"),
            socket_path: socket_dir.path().join("mcp.sock"),
        });
        let controlled_launch = Arc::new(tokio::sync::Notify::new());
        let finish_launch = Arc::new(tokio::sync::Notify::new());
        let launch_entered = controlled_launch.clone();
        let launch_release = finish_launch.clone();
        let launches = Arc::new(AtomicUsize::new(0));
        let count = launches.clone();
        let hook: SpawnHook = Arc::new(move |_, _, _, _| {
            let count = count.clone();
            let entered = launch_entered.clone();
            let release = launch_release.clone();
            Box::pin(async move {
                if !withdrawn && !takeover {
                    entered.notify_one();
                    release.notified().await;
                }
                count.fetch_add(1, Ordering::SeqCst);
                Ok(SpawnHandle::NoOp)
            })
        });
        let mut adapter = ClaudeWorkerAdapter::new_with_spawn_hook(
            harness.repo.clone(),
            Arc::new(CodexClient::new_stub()),
            Some(server),
            harness.adapter.card_role_cache.clone(),
            harness.adapter.track_area_cache.clone(),
            harness.workspace.path().into(),
            hook,
        );
        let entered = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        let entered_hook = entered.clone();
        let resume_hook = resume.clone();
        adapter.preparation_hook = Some(Arc::new(move || {
            let entered = entered_hook.clone();
            let resume = resume_hook.clone();
            Box::pin(async move {
                entered.notify_one();
                resume.notified().await;
            })
        }));
        let adapter = Arc::new(adapter);
        let op_repo = Arc::new(SqlxOperationRepo::new(harness.repo.pool().clone()));
        let (kind, payload) = crate::scheduler::build_worker_payload(&fixture.task).unwrap();
        let id = op_repo
            .insert_operation(
                kind,
                OperationKey {
                    operation_key: new_id(),
                    idempotency_key: Some(fixture.task.id.clone()),
                    payload_hash: crate::routes::terminal_cards::stable_payload_hash(&payload)
                        .unwrap(),
                },
                payload,
            )
            .await
            .unwrap();
        let op = op_repo
            .claim_drive_batch(1)
            .await
            .unwrap()
            .into_iter()
            .find(|op| op.id == id)
            .unwrap();
        let (op, _) = op_repo
            .prepare_tx_and_advance(&op, adapter.as_ref())
            .await
            .unwrap()
            .unwrap();
        let prepared_id = op.id.clone();
        let leased = op_repo
            .claim_drive_batch(1)
            .await
            .unwrap()
            .into_iter()
            .find(|op| op.id == prepared_id)
            .unwrap();
        op_repo
            .set_phase(&leased, crate::operation::Phase::SpawnStarted)
            .await
            .unwrap()
            .unwrap();
        let op = op_repo
            .claim_drive_batch(1)
            .await
            .unwrap()
            .into_iter()
            .find(|op| op.id == prepared_id)
            .unwrap();
        let output = op.tx_output.clone().unwrap();
        let old_owner = op.lease_owner.clone();
        let ctx = SpawnCtx::new(
            harness.repo.clone(),
            op_repo.clone(),
            Arc::new(DaemonClient::new_stub()),
            TerminalRendererRegistry::new(),
            harness.events.clone(),
            OperationCompletionBus::new(),
        );
        let run = tokio::spawn(async move { adapter.spawn_side_effect(&output, &op, &ctx).await });
        struct AbortOnDrop(tokio::task::AbortHandle);
        impl Drop for AbortOnDrop {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _abort = AbortOnDrop(run.abort_handle());
        tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
            .await
            .expect("adapter reaches async preparation");
        if withdrawn {
            fixture.withdraw().await;
        }
        // Expiry is eligibility for takeover, not a different owner by itself.
        // Exercise both outcomes at the actual paused adapter boundary.
        sqlx::query("UPDATE operations SET lease_until_ms=0 WHERE id=?1")
            .bind(&prepared_id)
            .execute(harness.repo.pool())
            .await
            .unwrap();
        if takeover {
            let replacement = op_repo
                .claim_drive_batch(1)
                .await
                .unwrap()
                .into_iter()
                .find(|op| op.id == prepared_id)
                .unwrap();
            assert_ne!(replacement.lease_owner, old_owner);
        }
        resume.notify_one();
        if !withdrawn && !takeover {
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                controlled_launch.notified(),
            )
            .await
            .unwrap();
            let mut contender = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                harness.repo.pool().acquire(),
            )
            .await
            .unwrap()
            .unwrap();
            // Shared-cache SQLite waits via unlock_notify, irrespective of
            // busy_timeout. Keep the same query alive across the release so
            // this proves actual serialization without leaking a parked writer.
            let mut reservation = Box::pin(sqlx::query("BEGIN IMMEDIATE").execute(&mut *contender));
            let early =
                tokio::time::timeout(std::time::Duration::from_millis(100), &mut reservation).await;
            let serialized = early.is_err();
            finish_launch.notify_one();
            match early {
                Ok(result) => {
                    result.unwrap();
                }
                Err(_) => {
                    tokio::time::timeout(std::time::Duration::from_secs(5), &mut reservation)
                        .await
                        .unwrap()
                        .unwrap();
                }
            }
            drop(reservation);
            sqlx::query("ROLLBACK")
                .execute(&mut *contender)
                .await
                .unwrap();
            assert!(
                serialized,
                "withdrawal writer must remain serialized through the controlled launch boundary"
            );
        }
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), run)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            launches.load(Ordering::SeqCst),
            usize::from(!withdrawn && !takeover),
            "withdrawal={withdrawn}, takeover={takeover}: final launch error {:?}",
            result.as_ref().err()
        );
        assert_eq!(result.is_err(), withdrawn || takeover);
    }
}
