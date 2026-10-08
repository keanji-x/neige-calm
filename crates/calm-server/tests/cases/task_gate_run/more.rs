//! #2464 slice 1: the release order (R8d), who may run (R9), the checkpoint's refusal (R10), one
//! attempt commit (R12) and a stuck run (R13).
use super::*;

// ---------------------------------------------------------------------------
// R8d (D9): the run releases its held wrapper only after its op is parked.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_run_releases_its_wrapper_only_after_parked() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let record = seen.clone();
    let fx = world_with(LONG_IDLE, move |boot| {
        let pool = boot.repo.sqlite_pool().unwrap();
        let hook: Hook = Arc::new(move || {
            let (pool, record) = (pool.clone(), record.clone());
            Box::pin(async move {
                let phase: String =
                    sqlx::query_scalar("SELECT phase FROM operations WHERE kind = 'task-gate-run'")
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                record.lock().unwrap().push(phase);
            })
        });
        hooked(boot, |adapter| adapter.with_before_release(hook))
    })
    .await;
    let a = running(&fx, "r8d", json!([{"name": "fine", "cmd": "true"}])).await;

    passed(&gate_run(&fx, &a, None).await.unwrap());

    assert_eq!(*seen.lock().unwrap(), vec!["parked".to_string()]);
}

// ---------------------------------------------------------------------------
// R9 (D10): who may run; a refusal writes no row.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_gate_run_is_refused_for_other_callers_and_states() {
    let fx = world(LONG_IDLE).await;
    let a = running(&fx, "r9", json!([{"name": "fine", "cmd": "true"}])).await;
    let refused = |result: Result<Value, RpcError>, code: i64, needle: &str| {
        let err = result.expect_err("refused");
        assert_eq!(err.code, code, "{err:?}");
        assert!(err.message.contains(needle), "want {needle:?} in {err:?}");
    };

    // The Planner: the tool is the Worker's.
    refused(
        call_tool(
            &fx.boot,
            GATE,
            planner_identity(&fx.boot),
            json!({"attempt_id": a.task.id}),
        )
        .await,
        -32403,
        "requires role in [Worker]",
    );
    // Another worker: not the attempt it was handed.
    let other = fx.new_worker("r9-other", AgentProvider::Codex).await;
    refused(
        call_tool(&fx.boot, GATE, other, json!({"attempt_id": a.task.id})).await,
        -32602,
        "a gate run needs the running attempt you were handed",
    );
    // A read-only task with a gate: nothing for the kernel to commit. A declaration refuses the
    // pair, so the row is given its gate by hand (the shape the admission still guards).
    let reader = fx.new_worker("r9-reader", AgentProvider::Codex).await;
    let read_only = fx
        .running_task(
            "r9-reader",
            "codex",
            &reader.card_id,
            json!({"access": "read_only"}),
        )
        .await;
    sqlx::query("UPDATE tasks SET gate_json = ?1 WHERE id = ?2")
        .bind(json!({"steps": [{"name": "fine", "cmd": "true"}]}).to_string())
        .bind(&read_only.id)
        .execute(&fx.pool())
        .await
        .unwrap();
    refused(
        call_tool(&fx.boot, GATE, reader, json!({"attempt_id": read_only.id})).await,
        -32602,
        "task `r9-reader` has no kernel commit; its gate runs after you report done",
    );
    // An ungated task.
    let plain = fx.new_worker("r9-plain", AgentProvider::Codex).await;
    let ungated = fx
        .running_task("r9-plain", "codex", &plain.card_id, json!({}))
        .await;
    refused(
        call_tool(&fx.boot, GATE, plain, json!({"attempt_id": ungated.id})).await,
        -32602,
        "task `r9-plain` declares no gate",
    );
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE kind = ?1")
        .bind(TASK_GATE_RUN_KIND)
        .fetch_one(&fx.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0, "no refusal wrote an op row");

    let answer = gate_run(&fx, &a, None).await.unwrap();
    passed(&answer);
    assert_eq!(answer["run"], 1, "the first admitted call is r1: {answer}");

    // A verifying attempt: its run time is over.
    fx.complete(&a.worker, &a.task.id).await;
    assert_eq!(current(&fx.boot, "r9").await.status, TaskStatus::Verifying);
    refused(
        gate_run(&fx, &a, None).await,
        -32602,
        "a gate run needs the running attempt you were handed",
    );
    assert_eq!(run_ops(&fx, &a.task.id).await.len(), 1);
}

// ---------------------------------------------------------------------------
// R10 (P2): a checkout on another branch is refused by the checkpoint; nothing is committed.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_checkout_on_another_branch_is_refused_without_a_commit() {
    let fx = world(LONG_IDLE).await;
    let a = running(&fx, "r10", json!([{"name": "fine", "cmd": "true"}])).await;
    let path = &a.lease.path;
    git(path, &["checkout", "-q", "-b", "elsewhere"]);
    std::fs::write(path.join("r10.txt"), "not committed\n").unwrap();
    let head = git(path, &["rev-parse", "HEAD"]);

    let answer = gate_run(&fx, &a, Some("never committed")).await.unwrap();

    assert_eq!(answer["state"], "finished", "{answer}");
    assert_eq!(answer["status_detail"], "gate-target-mismatch", "{answer}");
    assert_eq!(answer["failing_step"], "neige-checkpoint", "{answer}");
    assert_eq!(answer["exit_code"], 11, "{answer}");
    assert_eq!(answer["commit"], Value::Null, "{answer}");
    assert!(
        answer["log_tail"]
            .as_str()
            .unwrap()
            .contains(&failure_sentence("11")),
        "{answer}"
    );
    assert_eq!(git(path, &["rev-parse", "HEAD"]), head, "HEAD unmoved");
    assert_eq!(git(path, &["symbolic-ref", "HEAD"]), "refs/heads/elsewhere");
    assert_eq!(git(path, &["status", "--porcelain"]), "?? r10.txt");
}

// ---------------------------------------------------------------------------
// R12 (D2, F1): runs keep one attempt commit above the lease base.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runs_keep_one_attempt_commit_above_the_lease_base() {
    let fx = world(LONG_IDLE).await;
    let a = running(&fx, "r12", json!([{"name": "fine", "cmd": "true"}])).await;
    let path = &a.lease.path;
    std::fs::write(path.join("a.txt"), "a\n").unwrap();
    let first = passed(&gate_run(&fx, &a, Some("M1")).await.unwrap()).to_string();
    // A worker that commits on its own (a Claude worker is not prevented from it).
    std::fs::write(path.join("self.txt"), "self\n").unwrap();
    git(path, &["add", "self.txt"]);
    git(path, &["commit", "-q", "-m", "a self-made commit"]);
    std::fs::write(path.join("b.txt"), "b\n").unwrap();

    let second = passed(&gate_run(&fx, &a, Some("M2")).await.unwrap()).to_string();

    assert_eq!(
        git(path, &["rev-list", &format!("{}..HEAD", a.lease.base_sha)]),
        second,
        "one commit above the lease base"
    );
    assert_eq!(git(path, &["log", "-1", "--format=%B"]), "M2");
    let files = git(path, &["ls-tree", "-r", "--name-only", &second]);
    for file in ["a.txt", "b.txt", "self.txt"] {
        assert!(files.lines().any(|line| line == file), "{file}: {files}");
    }
    assert!(
        git(path, &["branch", "--contains", &first]).is_empty(),
        "run 1's commit is off every branch"
    );
    assert_eq!(
        ref_target(
            &a.lease.git_common_dir,
            &format!(
                "refs/neige/gate-runs/{}/{}/r1",
                fx.track(),
                a.worker.card_id
            )
        ),
        Some(first),
        "run 1's ref still pins it"
    );
}

// ---------------------------------------------------------------------------
// R13 (D7, D8): a stuck run refuses the next run and a regate, but not the delivery.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stuck_run_does_not_hold_the_delivery() {
    let fx = world(LONG_IDLE).await;
    let red = outside(&fx, "r13-red");
    let a = running(
        &fx,
        "r13",
        json!([{"name": "env", "cmd": format!("test ! -f '{}'", red.display())}]),
    )
    .await;
    std::fs::write(a.lease.path.join("r13.txt"), "r13\n").unwrap();
    let commit = passed(&gate_run(&fx, &a, Some("Deliver r13")).await.unwrap()).to_string();
    sqlx::query(
        "UPDATE operations SET phase = 'stuck', \
         phase_detail_json = '{\"reason\":\"test stuck\",\"since\":1}' \
         WHERE kind = ?1 AND idempotency_key = ?2",
    )
    .bind(TASK_GATE_RUN_KIND)
    .bind(format!("{}#r1", a.task.id))
    .execute(&fx.pool())
    .await
    .unwrap();

    let err = gate_run(&fx, &a, None).await.unwrap_err();
    assert_eq!(err.code, -32409, "{err:?}");
    assert!(
        err.message
            .contains("run `r1`'s processes could not be proven stopped; report done or fail"),
        "{err:?}"
    );

    touch(&red);
    fx.complete(&a.worker, &a.task.id).await;
    fx.wait_forge_op(&a.task.id).await;
    let row = fx.delivery_row(&a.task.id).await.unwrap();
    fx.scheduler()
        .settle_git_delivery_for_test(&row.delivery_id)
        .await
        .unwrap();
    let row = fx.delivery_row(&a.task.id).await.unwrap();
    assert_eq!(row.settlement.as_deref(), Some("candidate"), "{row:?}");
    assert_eq!(
        fx.candidate_row(&a.task.id).await.unwrap().commit_sha,
        commit
    );

    let task = current(&fx.boot, "r13").await;
    fx.scheduler().drive_gate_for_test(task).await.unwrap();
    let task = current(&fx.boot, "r13").await;
    assert_eq!(
        (task.status, task.status_detail.as_deref()),
        (TaskStatus::Failed, Some("gate-red"))
    );
    let err = call_tool(
        &fx.boot,
        "neige_task_regate",
        planner_identity(&fx.boot),
        json!({"attempt_id": a.task.id, "message": "try again"}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32409, "{err:?}");
    assert!(
        err.message
            .contains("a gate op of this task is unfinished or stuck (run r1 is stuck)"),
        "{err:?}"
    );
}
