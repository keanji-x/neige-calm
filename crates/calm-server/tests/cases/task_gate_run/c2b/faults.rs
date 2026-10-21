//! Fault schedules at the real observer's I/O boundaries.
use super::*;
use calm_server::error::CalmError;
use calm_server::operation::{OperationRepo, SqlxOperationRepo};
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn c2b_lost_initial_release_response_persists_refusal_before_reconcile() {
    let (pause, entered, resume) = pause();
    let fx = world_with(Duration::from_millis(100), |b| {
        hooked(b, |a| {
            a.with_release_response(Arc::new(move || {
                let pause = pause.clone();
                Box::pin(async move {
                    pause().await;
                    Err(CalmError::Internal(
                        "lost initial CloseStdin response".into(),
                    ))
                })
            }))
        })
    })
    .await;
    let counter = outside(&fx, "lost-response-counter");
    let a = running(
        &fx,
        "lost-response",
        json!([{"name":"once","cmd":format!("echo once >> '{}'",counter.display())}]),
    )
    .await;
    gate_run(&fx, &a, Some("test: lost initial release response"))
        .await
        .unwrap();
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .unwrap();
    let before = op(&fx, &a.task.id).await;
    let id = record(&before)["execution_id"].as_str().unwrap().to_owned();
    // The child and Engine have settled before the release error reaches observe.
    wait_until(async || {
        sqlx::query_scalar::<_, bool>(
            "SELECT settlement_json IS NOT NULL FROM workspace_executions WHERE execution_id=?",
        )
        .bind(&id)
        .fetch_one(&fx.pool())
        .await
        .unwrap()
    })
    .await;
    assert_eq!(std::fs::read_to_string(&counter).unwrap(), "once\n");
    resume.notify_one();
    let result = run_result(&fx, &a.task.id, 1).await;
    let after = op(&fx, &a.task.id).await;
    assert_eq!(
        record(&after)["decision"]["Stopping"]["reason"],
        "ReleaseRefused"
    );
    assert!(!result.verdict.passed);
    assert_eq!(result.verdict.status_detail.as_deref(), Some("gate-infra"));
    assert!(
        serde_json::to_string(&result)
            .unwrap()
            .contains("release or authorization failed")
    );
    assert_eq!(execution_count(&fx, &after.id).await, 1);
    assert_eq!(std::fs::read_to_string(counter).unwrap(), "once\n");
}

#[tokio::test]
async fn c2b_observer_read_retry_and_sweep_completion_actually_retire() {
    let reads = Arc::new(AtomicUsize::new(0));
    let (pause, entered, resume) = pause();
    let count = reads.clone();
    let fx = world_with(Duration::from_millis(100), |b| {
        hooked(b, |a| {
            a.with_observer_read(Arc::new(move || {
                let n = count.fetch_add(1, Ordering::SeqCst);
                let pause = pause.clone();
                Box::pin(async move {
                    match n {
                        0 => Ok(()), // Normal release and first reconciliation.
                        1 => Err(CalmError::Internal("one-shot observer read failure".into())),
                        _ => {
                            pause().await;
                            Ok(())
                        }
                    }
                })
            }))
        })
    })
    .await;
    let flag = outside(&fx, "sweep-release");
    let a = running(
        &fx,
        "sweep-retire",
        json!([{"name":"wait","cmd":wait_for(&flag)}]),
    )
    .await;
    gate_run(&fx, &a, Some("test: sweep retirement"))
        .await
        .unwrap();
    // Only a retry after the injected error can reach this pause.
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .expect("observer retries transient read");
    assert_eq!(reads.load(Ordering::SeqCst), 3);
    let before = op(&fx, &a.task.id).await;
    let id = record(&before)["execution_id"].as_str().unwrap().to_owned();
    touch(&flag);
    wait_until(async || {
        sqlx::query_scalar::<_, bool>(
            "SELECT settlement_json IS NOT NULL FROM workspace_executions WHERE execution_id=?",
        )
        .bind(&id)
        .fetch_one(&fx.pool())
        .await
        .unwrap()
    })
    .await;
    assert_eq!(op(&fx, &a.task.id).await.phase.tag(), PhaseTag::Parked);
    fx.runtime.sweep_parked().await.unwrap();
    assert_eq!(op(&fx, &a.task.id).await.phase.tag(), PhaseTag::Succeeded);
    // The observer remains paused: completion itself must wake Retire.
    wait_until(async || {
        sqlx::query_scalar::<_, bool>(
            "SELECT secrets_json IS NULL FROM workspace_executions WHERE execution_id=?",
        )
        .bind(&id)
        .fetch_one(&fx.pool())
        .await
        .unwrap()
    })
    .await;
    let engine = fx.execution.lifecycle.engine().unwrap();
    assert!(engine.available());
    let id = serde_json::from_value(Value::String(id)).unwrap();
    assert!(matches!(
        engine.output(&id, None).await.unwrap(),
        workspace_runtime::execution::ExecutionReply::Retired(_)
    ));
    resume.notify_one();
}

async fn settled(fx: &Fx, operation: &Operation) {
    let id = record(operation)["execution_id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_until(async || {
        sqlx::query_scalar::<_, bool>(
            "SELECT settlement_json IS NOT NULL FROM workspace_executions WHERE execution_id=?",
        )
        .bind(&id)
        .fetch_one(&fx.pool())
        .await
        .unwrap()
    })
    .await;
}

async fn expire(fx: &Fx, operation: &Operation) {
    sqlx::query("UPDATE operations SET lease_until_ms=0 WHERE id=?")
        .bind(&operation.id)
        .execute(&fx.pool())
        .await
        .unwrap();
}

// Pause the response after real release I/O, and pause the next observer iteration.
// Sweeps run explicitly so each schedule has exactly one known competing claimant.
async fn release_handoff(fail_write: bool, replacement: bool, child_waits: bool) {
    let (response, entered, resume) = pause();
    let (retry, retry_entered, retry_resume) = pause();
    let reads = Arc::new(AtomicUsize::new(0));
    let fx = world_with(Duration::from_millis(100), |b| {
        hooked(b, |a| {
            a.with_release_response(Arc::new(move || {
                let response = response.clone();
                Box::pin(async move {
                    response().await;
                    if replacement && child_waits {
                        Ok(()) // A late successful response still cannot ack under an expired claim.
                    } else {
                        Err(CalmError::Internal("lost release response".into()))
                    }
                })
            }))
            .with_observer_read(Arc::new(move || {
                let n = reads.fetch_add(1, Ordering::SeqCst);
                let retry = retry.clone();
                Box::pin(async move {
                    if n > 0 {
                        retry().await;
                    }
                    Ok(())
                })
            }))
        })
    })
    .await;
    let flag = outside(&fx, "handoff-child");
    let command = if child_waits {
        wait_for(&flag)
    } else {
        "true".into()
    };
    let a = running(&fx, "handoff", json!([{"name":"step","cmd":command}])).await;
    gate_run(&fx, &a, Some("test: release claim handoff"))
        .await
        .unwrap();
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .unwrap();
    let before = op(&fx, &a.task.id).await;
    if !child_waits {
        settled(&fx, &before).await;
    }
    if fail_write {
        sqlx::query(
            "CREATE TRIGGER refuse_decision BEFORE UPDATE OF tx_output_json ON operations \
             WHEN json_extract(NEW.tx_output_json,'$.data.gate_execution.decision.Stopping.reason')='ReleaseRefused' \
             BEGIN SELECT RAISE(ABORT,'one-shot refusal persistence failure'); END",
        )
            .execute(&fx.pool()).await.unwrap();
        resume.notify_one();
        tokio::time::timeout(WAIT, retry_entered.notified())
            .await
            .unwrap();
        assert_eq!(record(&op(&fx, &a.task.id).await)["decision"], "Observing");
        sqlx::query("DROP TRIGGER refuse_decision")
            .execute(&fx.pool())
            .await
            .unwrap();
    }
    expire(&fx, &before).await;
    if replacement {
        let claim = SqlxOperationRepo::new(fx.db())
            .claim_parked(&before.id)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(claim.lease_owner, before.lease_owner);
        let claimed_record = record(&claim);
        resume.notify_one();
        tokio::time::timeout(WAIT, retry_entered.notified())
            .await
            .unwrap();
        let after = op(&fx, &a.task.id).await;
        assert_eq!(
            record(&after),
            claimed_record,
            "stale observer cannot write a decision"
        );
        assert_eq!(
            after.phase.tag(),
            PhaseTag::Parked,
            "stale observer cannot complete"
        );
        assert_eq!(
            after.lease_owner, claim.lease_owner,
            "stale observer cannot clear replacement claim"
        );
        touch(&flag);
        settled(&fx, &after).await;
        expire(&fx, &after).await;
    }
    fx.runtime.sweep_parked().await.unwrap();
    // The first sweep persists refusal; the next completes against that durable decision.
    fx.runtime.sweep_parked().await.unwrap();
    let result = run_result(&fx, &a.task.id, 1).await;
    assert!(
        !result.verdict.passed,
        "unacknowledged release must never be green"
    );
    assert_eq!(result.verdict.status_detail.as_deref(), Some("gate-infra"));
    let after = op(&fx, &a.task.id).await;
    assert_eq!(
        record(&after)["decision"]["Stopping"]["reason"],
        "ReleaseRefused"
    );
    if !fail_write && !replacement {
        resume.notify_one();
    }
    retry_resume.notify_one();
}

#[tokio::test]
async fn c2b_failed_refusal_persistence_sweep_handoff_never_green() {
    release_handoff(true, false, false).await;
}

#[tokio::test]
async fn c2b_pending_release_expired_claim_sweep_never_green() {
    release_handoff(false, false, false).await;
}

#[tokio::test]
async fn c2b_paused_release_observer_cannot_adopt_replacement_claim() {
    release_handoff(false, true, false).await;
    release_handoff(false, true, true).await;
}

#[tokio::test]
async fn c4_recovery_tick_settles_drained_gate_before_deadline() {
    let fx = world(Duration::from_millis(100)).await;
    fx.execution.faults.arm("settle");
    let a = running(&fx, "tick-settle", json!([{"name":"ok","cmd":"true"}])).await;
    let start = Instant::now();
    gate_run(&fx, &a, Some("test: tick settlement"))
        .await
        .unwrap();
    assert!(run_result(&fx, &a.task.id, 1).await.verdict.passed);
    assert!(
        start.elapsed() < Duration::from_secs(20),
        "must finish before 120s deadline"
    );
    assert_eq!(fx.execution.faults.failures.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn c4_recovery_tick_redrives_gate_retire_after_store_error() {
    let (pause, entered, resume) = pause();
    let reads = Arc::new(AtomicUsize::new(0));
    let fx = world_with(Duration::from_millis(100), |b| {
        hooked(b, |a| {
            a.with_observer_read(Arc::new(move || {
                let first = reads.fetch_add(1, Ordering::SeqCst) == 0;
                let pause = pause.clone();
                Box::pin(async move {
                    if !first {
                        pause().await;
                    }
                    Ok(())
                })
            }))
        })
    })
    .await;
    let flag = outside(&fx, "tick-retire-finish");
    let a = running(
        &fx,
        "tick-retire",
        json!([{"name":"ok","cmd":wait_for(&flag)}]),
    )
    .await;
    gate_run(&fx, &a, Some("test: tick retirement"))
        .await
        .unwrap();
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .unwrap();
    touch(&flag);
    settled(&fx, &op(&fx, &a.task.id).await).await;
    fx.execution.faults.arm("finish");
    fx.runtime.sweep_parked().await.unwrap();
    assert!(run_result(&fx, &a.task.id, 1).await.verdict.passed);
    let id: workspace_runtime::execution::ExecutionId =
        serde_json::from_value(record(&op(&fx, &a.task.id).await)["execution_id"].clone()).unwrap();
    wait_until(async || {
        sqlx::query_scalar::<_, bool>(
            "SELECT secrets_json IS NULL FROM workspace_executions WHERE execution_id=?",
        )
        .bind(id.as_str())
        .fetch_one(&fx.pool())
        .await
        .unwrap()
    })
    .await;
    resume.notify_one();
    assert_eq!(fx.execution.faults.failures.load(Ordering::SeqCst), 1);
    assert!(matches!(
        fx.execution
            .lifecycle
            .engine()
            .unwrap()
            .output(&id, None)
            .await
            .unwrap(),
        workspace_runtime::execution::ExecutionReply::Retired(_)
    ));
}
