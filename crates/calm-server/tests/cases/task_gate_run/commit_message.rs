//! #2617: the attempt owns its message; gate evidence stays bound to that message.
use super::*;

const MESSAGE: &str = "fix: preserve attempt ownership\n\nRefs #2617";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upgrade_legacy_generated_run_message_does_not_authorize_done() {
    let fx = world(LONG_IDLE).await;
    let a = running(&fx, "legacy-only", json!([{"name":"ok", "cmd":"true"}])).await;
    // The old kernel persisted this exact payload when commit_message was omitted.
    let legacy = format!("neige: attempt {} gate run 1", a.task.id);
    std::fs::write(a.lease.path.join("before"), "before").unwrap();
    let commit = passed(&gate_run(&fx, &a, Some(&legacy)).await.unwrap()).to_string();
    std::fs::write(a.lease.path.join("after"), "after").unwrap();
    let err = done(&fx, &a, None).await.unwrap_err();
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("commit_message"), "{err:?}");
    assert_eq!(
        current(&fx.boot, "legacy-only").await.status,
        TaskStatus::Running
    );
    assert!(fx.delivery_row(&a.task.id).await.is_none());
    assert_eq!(git(&a.lease.path, &["rev-parse", "HEAD"]), commit);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upgrade_legacy_generated_run_message_does_not_shadow_explicit_input() {
    for later_run in [false, true] {
        let fx = world(LONG_IDLE).await;
        let a = running(&fx, "legacy-shadow", json!([{"name":"ok", "cmd":"true"}])).await;
        passed(&gate_run(&fx, &a, Some(MESSAGE)).await.unwrap());
        let legacy = format!("neige: attempt {} gate run 2", a.task.id);
        passed(&gate_run(&fx, &a, Some(&legacy)).await.unwrap());
        std::fs::write(a.lease.path.join("after"), "after").unwrap();
        if later_run {
            passed(&gate_run(&fx, &a, None).await.unwrap());
            assert_eq!(git(&a.lease.path, &["log", "-1", "--format=%B"]), MESSAGE);
        }
        assert_eq!(
            done(&fx, &a, None).await.unwrap()["delivery_commit_message"],
            MESSAGE
        );
        settle(&fx, &a).await;
        assert_eq!(git(&a.lease.path, &["log", "-1", "--format=%B"]), MESSAGE);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upgrade_legacy_skip_requires_exact_attempt_run_and_bytes() {
    for variant in 0..3 {
        let fx = world(LONG_IDLE).await;
        let a = running(&fx, "legacy-exact", json!([{"name":"ok", "cmd":"true"}])).await;
        let message = match variant {
            0 => "neige: attempt another-attempt gate run 1".to_string(),
            1 => format!("neige: attempt {} gate run 2", a.task.id),
            _ => format!("neige: attempt {} gate run 1\n", a.task.id),
        };
        passed(&gate_run(&fx, &a, Some(&message)).await.unwrap());
        std::fs::write(a.lease.path.join("after"), "after").unwrap();
        assert_eq!(
            done(&fx, &a, None).await.unwrap()["delivery_commit_message"],
            message
        );
        settle(&fx, &a).await;
        assert_eq!(
            git(&a.lease.path, &["log", "-1", "--format=%B"]),
            message.trim_end()
        );
    }
}

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
    let equivalent = gate_run(&fx, &a, Some(&format!("{MESSAGE} \t\n\n")))
        .await
        .unwrap();
    assert_eq!(equivalent["run"], 1);
    let refused = gate_run(&fx, &a, Some("different message")).await;
    touch(&flag);
    run_result(&fx, &a.task.id, 1).await;
    let err = refused.unwrap_err();
    assert_eq!(err.code, -32409);
    assert!(err.message.contains("r1"), "{err:?}");
    assert_eq!(run_ops(&fx, &a.task.id).await.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn done_reports_new_delivery_message_without_rewriting_the_run() {
    for edit in [false, true] {
        let fx = world(LONG_IDLE).await;
        let a = running(&fx, "different", json!([{"name":"ok", "cmd":"true"}])).await;
        std::fs::write(a.lease.path.join("one"), "one").unwrap();
        let commit = passed(&gate_run(&fx, &a, Some(MESSAGE)).await.unwrap()).to_string();
        if edit {
            std::fs::write(a.lease.path.join("two"), "two").unwrap();
        }
        let result = done(&fx, &a, Some("different message")).await.unwrap();
        assert_eq!(result["delivery_commit_message"], "different message");
        assert_eq!(result["gate_run_commits_keep_original_message"], true);
        settle(&fx, &a).await;
        let candidate = fx.candidate_row(&a.task.id).await.unwrap();
        assert_eq!(candidate.commit_sha == commit, !edit);
        assert_eq!(
            git(&a.lease.path, &["log", "-1", "--format=%B"]),
            if edit { "different message" } else { MESSAGE }
        );
        assert_eq!(run_ops(&fx, &a.task.id).await.len(), 1);
    }
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
async fn done_inherits_latest_admitted_input_across_restart() {
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
        assert_eq!(stored, "second message");
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
            MESSAGE
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exhausted_runs_allow_done_without_a_message() {
    let fx = world(LONG_IDLE).await;
    let a = running(&fx, "cap-message", json!([{"name":"ok", "cmd":"true"}])).await;
    std::fs::write(a.lease.path.join("cap.txt"), "cap").unwrap();
    for _ in 0..5 {
        passed(&gate_run(&fx, &a, Some(MESSAGE)).await.unwrap());
    }
    let err = gate_run(&fx, &a, Some("different message"))
        .await
        .unwrap_err();
    assert!(
        err.message.contains("report done without commit_message"),
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
async fn done_before_checkpoint_release_accepts_the_admitted_input() {
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
        let result = done(&fx, &a, None).await;
        resume.notify_one();
        assert_eq!(result.unwrap()["delivery_commit_message"], "second message");
    });
    run.unwrap();
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
async fn noop_run_input_is_inherited_after_an_edit() {
    let fx = world(LONG_IDLE).await;
    let a = running(&fx, "noop-message", json!([{"name":"ok", "cmd":"true"}])).await;
    passed(&gate_run(&fx, &a, Some(MESSAGE)).await.unwrap());
    std::fs::write(a.lease.path.join("after-noop"), "edit").unwrap();
    let result = done(&fx, &a, None).await.unwrap();
    assert_eq!(result["delivery_commit_message"], MESSAGE);
    settle(&fx, &a).await;
    assert_eq!(git(&a.lease.path, &["log", "-1", "--format=%B"]), MESSAGE);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn noop_run_then_done_with_same_input_uses_only_one_run() {
    let fx = world(LONG_IDLE).await;
    let a = running(&fx, "noop-same", json!([{"name":"ok", "cmd":"true"}])).await;
    passed(&gate_run(&fx, &a, Some(MESSAGE)).await.unwrap());
    assert_eq!(
        done(&fx, &a, Some(MESSAGE)).await.unwrap()["delivery_commit_message"],
        MESSAGE
    );
    settle(&fx, &a).await;
    assert_eq!(run_ops(&fx, &a.task.id).await.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreign_base_message_never_controls_run_or_done() {
    for base_message in ["x".repeat(16_385), "foreign\u{1}message".to_string()] {
        for later_run in [false, true] {
            let fx = fixture_on_with_adapters(
                boot().await,
                |tmp| {
                    let repo = tmp.join("repo");
                    init_repo(&repo);
                    git(&repo, &["commit", "--amend", "-m", &base_message]);
                    repo
                },
                |_| Vec::new(),
            )
            .await;
            fx.dispatcher.abort_event_listener_for_test();
            let a = running(&fx, "foreign-base", json!([{"name":"ok", "cmd":"true"}])).await;
            // This run pins the foreign base, but its input remains MESSAGE.
            passed(&gate_run(&fx, &a, Some(MESSAGE)).await.unwrap());
            std::fs::write(a.lease.path.join("edit"), "edit").unwrap();
            if later_run {
                passed(&gate_run(&fx, &a, None).await.unwrap());
            }
            done(&fx, &a, None).await.unwrap();
            settle(&fx, &a).await;
            assert_eq!(git(&a.lease.path, &["log", "-1", "--format=%B"]), MESSAGE);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn commit_created_before_ref_failure_keeps_input_inheritance() {
    let fx = world(LONG_IDLE).await;
    let a = running(&fx, "ref-failure", json!([{"name":"ok", "cmd":"true"}])).await;
    std::fs::write(a.lease.path.join("first"), "first").unwrap();
    let common = PathBuf::from(git(
        &a.lease.path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ));
    let ref_name = format!(
        "refs/neige/gate-runs/{}/{}/r1",
        fx.track(),
        a.worker.card_id
    );
    let lock = common.join(format!("{ref_name}.lock"));
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    std::fs::write(&lock, "held").unwrap();
    let result = gate_run(&fx, &a, Some(MESSAGE)).await.unwrap();
    assert_eq!(result["passed"], false);
    assert_eq!(result["failing_step"], "neige-checkpoint");
    assert_eq!(git(&a.lease.path, &["log", "-1", "--format=%B"]), MESSAGE);
    std::fs::remove_file(lock).unwrap();
    std::fs::write(a.lease.path.join("after"), "after").unwrap();
    let result = done(&fx, &a, None).await.unwrap();
    assert_eq!(result["delivery_commit_message"], MESSAGE);
    settle(&fx, &a).await;
    assert_eq!(git(&a.lease.path, &["log", "-1", "--format=%B"]), MESSAGE);
}
