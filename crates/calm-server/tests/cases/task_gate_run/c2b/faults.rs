//! Fault schedules at the real observer's I/O boundaries.
use super::*;
use calm_server::error::CalmError;
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
