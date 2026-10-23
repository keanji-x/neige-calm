//! Both production recovery entries must wait for the receipt writer before loading a checkpoint.
use super::*;
use calm_server::harness::{HarnessPhaseTag, HarnessSnapshot, Observation, QueueEntry};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn boot_checkpoint_waits_for_settlement_and_keeps_full_identity() {
    checkpoint_after_settlement(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn operation_checkpoint_waits_for_settlement_and_keeps_full_identity() {
    checkpoint_after_settlement(true).await;
}

async fn checkpoint_after_settlement(operation_replay: bool) {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (track, card) = create(&stack).await;
    let runtime = stack.runtime(&card).await;
    let pool = stack.repo().sqlite_db().unwrap();
    let operation: String = sqlx::query_scalar("SELECT id FROM operations WHERE target_id=?1 AND phase='succeeded' ORDER BY created_at_ms DESC LIMIT 1")
        .bind(&card).fetch_one(pool.read_pool()).await.unwrap();
    let pause = calm_server::acp_planner::test_seams::pause_settlement(&runtime.id);
    std::fs::write(root.path().join("scenario"), "checkpoint").unwrap();
    let (status, body) = stack
        .post_input(&card, "original receipt-owned input")
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_file(&root, "setup-checkpoint").await;
    let mut checkpoint =
        HarnessSnapshot::from_value_strict(stack.runtime(&card).await.handle_state_json.unwrap());
    assert_eq!(checkpoint.phase, HarnessPhaseTag::IssuingTurn);
    assert!(checkpoint.projection_client_id.is_some());
    assert_eq!(checkpoint.pending_entries().len(), 1);
    std::fs::write(root.path().join("release-setup"), "").unwrap();
    tokio::time::timeout(Duration::from_secs(20), pause.entered.notified())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            stack
                .state
                .harness
                .reserve_after_shutdown(runtime.id.clone()),
        )
        .await
        .is_err()
    );
    assert!(
        stack.state.harness.get(&runtime.id).is_some(),
        "cancelled reservation must retain the receipt writer for the caller retry"
    );
    if operation_replay {
        sqlx::query("UPDATE operations SET phase='spawn_started',phase_detail_json=NULL,lease_owner=NULL,lease_until_ms=NULL WHERE id=?1")
            .bind(&operation).execute(pool.writer_pool()).await.unwrap();
    }
    let recover = || async {
        if operation_replay {
            calm_server::recover_operations_on_boot(&stack.state)
                .await
                .unwrap();
        } else {
            assert_eq!(
                calm_server::recover_harnesses_after_daemon_boot(
                    &stack.state,
                    Err(calm_server::error::CalmError::Internal(
                        "fixture: daemon down".into()
                    )),
                )
                .await
                .unwrap(),
                1
            );
        }
    };
    let mut recovery = Box::pin(recover());
    let premature = tokio::time::timeout(Duration::from_millis(150), recovery.as_mut())
        .await
        .is_ok();
    if premature {
        drop(recovery);
        pause.release.notify_one();
        stack.shutdown().await;
        panic!("checkpoint recovery returned before the receipt writer settled");
    }
    // Model the durable post-dispatch checkpoint left by a projection crash. Write it
    // after shutdown's snapshot persistence, while the actual receipt writer is paused.
    // The original queue proof/key and the later event watermark are one checkpoint;
    // merging only its queue into an old operation or boot snapshot cannot retire it.
    // Keep polling recovery while injecting the crash checkpoint. A timed-out
    // borrowed future may still own the writer connection between SQL awaits;
    // parking it here would deadlock this write against our own recovery future.
    let (checkpoint, later) = tokio::select! {
        biased;
        _ = recovery.as_mut() => {
            pause.release.notify_one();
            panic!("checkpoint recovery returned before the receipt writer settled");
        }
        injected = async {
            checkpoint.push_watermark = sqlx::query_scalar("SELECT MAX(id) FROM events")
                .fetch_one(pool.read_pool())
                .await
                .unwrap();
            assert!(checkpoint.push_watermark > 0);
            let later = QueueEntry::system(
                Observation::ReportEdited {
                    track_id: track.into(),
                    body_sha256: "later".into(),
                    body: "later report".into(),
                    author: Some(calm_server::event::EditAuthor::User),
                    body_before: None,
                    doc_rev_after: None,
                    blocks_after: None,
                },
                Some(checkpoint.push_watermark),
            )
            .unwrap();
            let mut entries = checkpoint.pending_entries();
            entries.push(later.clone());
            checkpoint.set_pending_entries(entries);
            sqlx::query("UPDATE worker_sessions SET handle_state_json=?2 WHERE id=?1")
                .bind(&runtime.id)
                .bind(serde_json::to_string(&checkpoint).unwrap())
                .execute(pool.writer_pool())
                .await
                .unwrap();
            (checkpoint, later)
        } => injected,
    };
    pause.release.notify_one();
    tokio::time::timeout(Duration::from_secs(20), recovery)
        .await
        .unwrap();
    let restored = stack.harness(&runtime.id).snapshot().await.unwrap();
    assert_eq!(restored.pending_entries(), vec![later]);
    assert_eq!(restored.projection_client_id, None);
    assert_eq!(restored.push_watermark, checkpoint.push_watermark);
    assert_eq!(
        restored.last_turn_id.as_deref(),
        Some(
            stack.wait_outcomes(&card, 1).await[0]["id"]
                .as_str()
                .unwrap()
        )
    );
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    assert_eq!(requests(&root, "session/new").len(), 1);
    assert!(requests(&root, "session/load").is_empty());
    assert!(requests(&root, "session/resume").is_empty());
    stack.shutdown().await;
}
