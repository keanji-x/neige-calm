//! #2617: the attempt owns its message; gate evidence stays bound to that message.
use super::*;

const MESSAGE: &str = "fix: preserve attempt ownership\n\nRefs #2617";

async fn settle(fx: &Fx, a: &Attempt) {
    fx.wait_forge_op(&a.task.id).await;
    let row = fx.delivery_row(&a.task.id).await.unwrap();
    fx.scheduler()
        .settle_git_delivery_for_test(&row.delivery_id)
        .await
        .unwrap();
}

async fn done(fx: &Fx, a: &Attempt, message: Option<&str>) -> Result<Value, RpcError> {
    let mut args = json!({"attempt_id": a.task.id});
    if let Some(message) = message {
        args["commit_message"] = json!(message);
    }
    call_tool(&fx.boot, "neige_task_done", a.worker.clone(), args).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_run_without_a_message_inherits_the_attempt_message() {
    let fx = world(LONG_IDLE).await;
    let a = running(&fx, "inherit", json!([{"name":"ok", "cmd":"true"}])).await;
    std::fs::write(a.lease.path.join("one"), "one").unwrap();
    passed(&gate_run(&fx, &a, Some(MESSAGE)).await.unwrap());
    std::fs::write(a.lease.path.join("two"), "two").unwrap();
    passed(&gate_run(&fx, &a, None).await.unwrap());
    assert_eq!(git(&a.lease.path, &["log", "-1", "--format=%B"]), MESSAGE);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_first_run_without_any_message_is_refused() {
    let fx = world(LONG_IDLE).await;
    let a = running(&fx, "missing", json!([{"name":"ok", "cmd":"true"}])).await;
    let err = gate_run(&fx, &a, None).await.unwrap_err();
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("commit_message"), "{err:?}");
    assert!(run_ops(&fx, &a.task.id).await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_joining_call_with_a_different_message_is_refused() {
    let fx = world(Duration::from_secs(2)).await;
    let flag = outside(&fx, "join-release");
    let a = running(
        &fx,
        "join",
        json!([{"name":"block", "cmd":wait_for(&flag)}]),
    )
    .await;
    let first = gate_run(&fx, &a, Some(MESSAGE)).await.unwrap();
    assert_eq!(first["state"], "running");
    let refused = gate_run(&fx, &a, Some("different message")).await;
    touch(&flag);
    run_result(&fx, &a.task.id, 1).await;
    let err = refused.unwrap_err();
    assert_eq!(err.code, -32409);
    assert!(err.message.contains("r1"), "{err:?}");
    assert_eq!(run_ops(&fx, &a.task.id).await.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn done_with_a_message_other_than_the_last_runs_is_refused() {
    let fx = world(LONG_IDLE).await;
    let a = running(&fx, "different", json!([{"name":"ok", "cmd":"true"}])).await;
    std::fs::write(a.lease.path.join("one"), "one").unwrap();
    passed(&gate_run(&fx, &a, Some(MESSAGE)).await.unwrap());
    let err = done(&fx, &a, Some("different message")).await.unwrap_err();
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("gate again"), "{err:?}");
    assert_eq!(
        current(&fx.boot, "different").await.status,
        TaskStatus::Running
    );
    assert_eq!(fx.delivery_count(&a.task.id).await, 0);
    assert!(fx.events_for("task.completed", &a.task.id).await.is_empty());
    // A new run is the only way to change the message, even with an unchanged tree.
    let changed = gate_run(&fx, &a, Some("different message")).await.unwrap();
    let commit = passed(&changed);
    assert_eq!(
        git(&a.lease.path, &["log", "-1", "--format=%B"]),
        "different message"
    );
    done(&fx, &a, Some("different message")).await.unwrap();
    settle(&fx, &a).await;
    assert_eq!(
        fx.candidate_row(&a.task.id).await.unwrap().commit_sha,
        commit
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_edit_after_the_run_commits_with_the_attempt_message() {
    let fx = world(LONG_IDLE).await;
    let a = running(&fx, "edit", json!([{"name":"ok", "cmd":"true"}])).await;
    std::fs::write(a.lease.path.join("one"), "one").unwrap();
    let first = passed(&gate_run(&fx, &a, Some(MESSAGE)).await.unwrap()).to_string();
    std::fs::write(a.lease.path.join("two"), "two").unwrap();
    done(&fx, &a, None).await.unwrap();
    settle(&fx, &a).await;
    let candidate = fx.candidate_row(&a.task.id).await.unwrap();
    assert_ne!(candidate.commit_sha, first);
    assert_eq!(git(&a.lease.path, &["log", "-1", "--format=%B"]), MESSAGE);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn done_with_the_run_message_still_reuses_the_run() {
    let fx = world(LONG_IDLE).await;
    let a = running(&fx, "same", json!([{"name":"ok", "cmd":"true"}])).await;
    std::fs::write(a.lease.path.join("one"), "one").unwrap();
    let commit = passed(&gate_run(&fx, &a, Some(MESSAGE)).await.unwrap()).to_string();
    done(&fx, &a, Some(MESSAGE)).await.unwrap();
    settle(&fx, &a).await;
    fx.scheduler()
        .drive_gate_for_test(current(&fx.boot, "same").await)
        .await
        .unwrap();
    assert_eq!(
        fx.candidate_row(&a.task.id).await.unwrap().commit_sha,
        commit
    );
    assert_eq!(
        reused_run(&verdict(&fx, "same").await),
        Some(format!("{}#r1", a.task.id).as_str())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn done_ignores_an_admitted_uncheckpointed_message_across_restart() {
    for restart in [false, true] {
        let mut fx = world(LONG_IDLE).await;
        let a = running(&fx, "pending-message", json!([{"name":"ok", "cmd":"true"}])).await;
        std::fs::write(a.lease.path.join("one"), "one").unwrap();
        let commit = passed(&gate_run(&fx, &a, Some(MESSAGE)).await.unwrap()).to_string();
        // Persist the real admission shape, but do not drive prepare/release before done.
        calm_server::test_seams::admit_gate_run_for_test(
            &fx.pool(),
            &a.task.id,
            &a.worker.card_id,
            fx.track(),
            calm_server::decision_sink::CommitMessage::parse("second message").unwrap(),
        )
        .await
        .unwrap();
        done(&fx, &a, None).await.unwrap();
        let stored: String = sqlx::query_scalar(
            "SELECT commit_message FROM task_git_deliveries WHERE producer_attempt_id = ?1",
        )
        .bind(&a.task.id)
        .fetch_one(&fx.pool())
        .await
        .unwrap();
        assert_eq!(stored, MESSAGE);
        if restart {
            fx.reboot().await;
        } else {
            fx.runtime.drive().await.unwrap();
            fx.scheduler()
                .schedule_track(fx.boot.track_id.clone())
                .await;
        }
        settle(&fx, &a).await;
        let candidate = fx.candidate_row(&a.task.id).await.unwrap();
        assert_eq!(candidate.commit_sha, commit);
        assert_eq!(
            git(
                &a.lease.path,
                &["show", "-s", "--format=%B", &candidate.commit_sha]
            ),
            stored
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exhausted_runs_allow_done_with_the_checkpointed_message_or_omission() {
    let fx = world(LONG_IDLE).await;
    let a = running(&fx, "cap-message", json!([{"name":"ok", "cmd":"true"}])).await;
    std::fs::write(a.lease.path.join("cap.txt"), "cap").unwrap();
    for _ in 0..5 {
        passed(&gate_run(&fx, &a, Some(MESSAGE)).await.unwrap());
    }
    let err = done(&fx, &a, Some("different message")).await.unwrap_err();
    assert!(err.message.contains("0 runs left"), "{err:?}");
    assert!(
        err.message
            .contains("omitting commit_message keeps the last checkpointed run's message"),
        "{err:?}"
    );
    let err = gate_run(&fx, &a, Some("different message"))
        .await
        .unwrap_err();
    assert!(
        err.message
            .contains("report done without commit_message or with that message"),
        "{err:?}"
    );
    done(&fx, &a, None).await.unwrap();
    settle(&fx, &a).await;
    assert_eq!(git(&a.lease.path, &["log", "-1", "--format=%B"]), MESSAGE);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn done_accepts_git_cleanup_equivalent_message() {
    let fx = world(LONG_IDLE).await;
    let a = running(&fx, "cleanup-message", json!([{"name":"ok", "cmd":"true"}])).await;
    std::fs::write(a.lease.path.join("cleanup.txt"), "cleanup").unwrap();
    passed(&gate_run(&fx, &a, Some(MESSAGE)).await.unwrap());
    done(&fx, &a, Some(&format!("{MESSAGE} \t\n\n")))
        .await
        .unwrap();
    settle(&fx, &a).await;
    assert_eq!(git(&a.lease.path, &["log", "-1", "--format=%B"]), MESSAGE);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn done_before_checkpoint_release_is_recoverably_refused() {
    let (hold, entered, resume) = pause();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fx = world_with(LONG_IDLE, |boot| {
        let hook: Hook = Arc::new(move || {
            let (hold, calls) = (hold.clone(), calls.clone());
            Box::pin(async move {
                if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0 {
                    hold().await;
                }
            })
        });
        hooked(boot, |adapter| adapter.with_before_release(hook))
    })
    .await;
    let a = running(
        &fx,
        "before-checkpoint",
        json!([{"name":"ok", "cmd":"true"}]),
    )
    .await;
    std::fs::write(a.lease.path.join("first.txt"), "first").unwrap();
    passed(&gate_run(&fx, &a, Some(MESSAGE)).await.unwrap());
    std::fs::write(a.lease.path.join("second.txt"), "second").unwrap();
    let (run, ()) = tokio::join!(gate_run(&fx, &a, Some("second message")), async {
        entered.notified().await;
        let refusal = done(&fx, &a, None).await;
        resume.notify_one();
        let err = refusal.expect_err("no selected message until the checkpoint is pinned");
        assert!(
            err.message.contains("wait for the run to finish"),
            "{err:?}"
        );
        assert_eq!(fx.delivery_count(&a.task.id).await, 0);
    });
    passed(&run.unwrap());
    done(&fx, &a, None).await.unwrap();
    settle(&fx, &a).await;
    let stored: String = sqlx::query_scalar(
        "SELECT commit_message FROM task_git_deliveries WHERE producer_attempt_id = ?1",
    )
    .bind(&a.task.id)
    .fetch_one(&fx.pool())
    .await
    .unwrap();
    assert_eq!(stored, "second message");
    assert_eq!(git(&a.lease.path, &["log", "-1", "--format=%B"]), stored);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn done_inherits_the_recorded_commits_message_for_a_noop_checkpoint() {
    let fx = world(LONG_IDLE).await;
    let a = running(&fx, "noop-message", json!([{"name":"ok", "cmd":"true"}])).await;
    let result = gate_run(&fx, &a, Some(MESSAGE)).await.unwrap();
    let commit = passed(&result).to_string();
    done(&fx, &a, None).await.unwrap();
    settle(&fx, &a).await;
    let stored: String = sqlx::query_scalar(
        "SELECT commit_message FROM task_git_deliveries WHERE producer_attempt_id = ?1",
    )
    .bind(&a.task.id)
    .fetch_one(&fx.pool())
    .await
    .unwrap();
    assert_eq!(
        git(&a.lease.path, &["show", "-s", "--format=%B", &commit]),
        stored
    );
}
