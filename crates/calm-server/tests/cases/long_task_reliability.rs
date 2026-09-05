use super::*;

#[tokio::test]
async fn long_task_gate_uses_released_worker_checkout() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    let boot = boot().await;
    set_lifecycle(&boot, TrackLifecycle::Working).await;
    let shared = tempfile::tempdir().unwrap();
    let checkout = tempfile::tempdir().unwrap();
    std::fs::write(checkout.path().join("worker-evidence"), "verified").unwrap();
    std::fs::write(shared.path().join("unrelated-user-change"), "dirty").unwrap();
    let gate = json!({"steps": [{"name": "checkout", "cmd":
        "test -f worker-evidence && test ! -f unrelated-user-change"}]})
    .to_string();
    let mut task = gate_task(&boot, "checkout", &gate);
    task.cwd = Some(shared.path().to_string_lossy().into_owned());
    task.worker_card_id = Some(boot.worker_card_id.to_string());
    seed_task(&boot, task).await;
    sqlx::query("INSERT INTO workspace_leases (lease_id, path, card_id, track_id, state, lease_owner, lease_until_ms, created_at_ms, updated_at_ms) VALUES ('checkout', ?1, ?2, ?3, 'released', 'test', 1, 1, 1)")
        .bind(checkout.path().to_str().unwrap())
        .bind(boot.worker_card_id.as_str()).bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap()).await.unwrap();
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(TaskVerifyAdapter::new(
            checkout.path().to_path_buf(),
        ))],
    );
    scheduler.schedule_track(boot.track_id.clone()).await;
    let row = wait_for_terminal_row(&boot, "checkout", 30).await;
    assert_eq!(row.status, TaskStatus::Done, "{row:?}");
    let verdict: Value = serde_json::from_str(row.gate_result_json.as_deref().unwrap()).unwrap();
    assert_eq!(verdict["cwd"], checkout.path().to_str().unwrap());
}

#[tokio::test]
async fn long_task_late_success_cannot_contradict_spawn_failure() {
    let boot = boot().await;
    set_lifecycle(&boot, TrackLifecycle::Working).await;
    let mut task = plan_task(&boot.track_id, "late", TaskKind::Codex, &[]);
    task.status = TaskStatus::Failed;
    task.status_detail = Some("spawn-failed".into());
    task.worker_card_id = Some(boot.worker_card_id.to_string());
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    bind_worker_card_payload(&boot, &task_id).await;
    seed_worker_op_target(
        &boot,
        "codex-worker",
        &task_id,
        boot.worker_card_id.as_str(),
    )
    .await;
    let result = call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        worker_identity(&boot),
        json!({"idempotency_key": task_id, "result": {"late": true}}),
    )
    .await;
    assert!(
        result.is_err(),
        "a losing report must surface the terminal conflict"
    );
    assert!(event_rows(&boot, "task.completed").await.is_empty());
    assert_eq!(track_lifecycle(&boot).await, TrackLifecycle::Working);
    assert_eq!(
        task_row(&boot, "late").await.status_detail.as_deref(),
        Some("spawn-failed")
    );
}

#[tokio::test]
async fn long_task_scheduled_worker_cannot_report_under_card_id() {
    let boot = boot().await;
    let mut task = plan_task(&boot.track_id, "identity", TaskKind::Codex, &[]);
    task.status = TaskStatus::Running;
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    bind_worker_card_payload(&boot, &task_id).await;
    seed_worker_op_target(
        &boot,
        "codex-worker",
        &task_id,
        boot.worker_card_id.as_str(),
    )
    .await;
    let result = call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        worker_identity(&boot),
        json!({"idempotency_key": boot.worker_card_id, "result": {}}),
    )
    .await;
    assert!(
        result.is_err(),
        "scheduled workers must echo their immutable task identity"
    );
    assert!(event_rows(&boot, "task.completed").await.is_empty());
    assert_eq!(
        task_row(&boot, "identity").await.status,
        TaskStatus::Running
    );
}

#[tokio::test]
async fn long_task_missing_worker_checkout_never_falls_back() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    for missing_lease in [true, false] {
        let boot = boot().await;
        let shared = tempfile::tempdir().unwrap();
        let gate =
            json!({"steps": [{"name": "must-not-run", "cmd": "touch gate-ran"}]}).to_string();
        let mut task = gate_task(&boot, "missing", &gate);
        task.cwd = Some(shared.path().to_str().unwrap().into());
        task.worker_card_id = Some(boot.worker_card_id.to_string());
        seed_task(&boot, task).await;
        if !missing_lease {
            sqlx::query("INSERT INTO workspace_leases (lease_id, path, card_id, track_id, state, lease_owner, created_at_ms, updated_at_ms) VALUES ('missing', ?1, ?2, ?3, 'released', 'test', 1, 1)")
                .bind(shared.path().join("missing-checkout").to_str().unwrap())
                .bind(boot.worker_card_id.as_str()).bind(boot.track_id.as_str())
                .execute(&boot.repo.sqlite_pool().unwrap()).await.unwrap();
        }
        let (_runtime, scheduler) = build_scheduler(
            &boot,
            vec![Arc::new(TaskVerifyAdapter::new(
                shared.path().to_path_buf(),
            ))],
        );
        scheduler.schedule_track(boot.track_id.clone()).await;
        let row = wait_for_terminal_row(&boot, "missing", 30).await;
        assert_eq!(row.status_detail.as_deref(), Some("gate-infra"), "{row:?}");
        assert!(!shared.path().join("gate-ran").exists());
    }
}

#[tokio::test]
async fn long_task_terminal_gate_uses_frozen_spawn_cwd() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    let boot = boot().await;
    let shared = tempfile::tempdir().unwrap();
    let checkout = tempfile::tempdir().unwrap();
    std::fs::write(checkout.path().join("terminal-evidence"), "ok").unwrap();
    let gate = json!({"steps": [{"name": "cwd", "cmd": "test -f terminal-evidence"}]}).to_string();
    let mut task = gate_task(&boot, "terminal-cwd", &gate);
    task.kind = TaskKind::Terminal;
    task.cwd = Some(shared.path().to_str().unwrap().into());
    task.worker_card_id = Some(boot.worker_card_id.to_string());
    let task_id = task.id.clone();
    seed_task(&boot, task).await;
    seed_worker_op_target(
        &boot,
        "terminal-worker",
        &task_id,
        boot.worker_card_id.as_str(),
    )
    .await;
    let mut output = TxOutput::new("card", Some(boot.worker_card_id.to_string()), json!({}));
    output.data = json!({"cwd": checkout.path()});
    sqlx::query("UPDATE operations SET phase = 'succeeded', tx_output_json = ?1 WHERE kind = 'terminal-worker' AND idempotency_key = ?2")
        .bind(serde_json::to_string(&output).unwrap()).bind(&task_id)
        .execute(&boot.repo.sqlite_pool().unwrap()).await.unwrap();
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(TaskVerifyAdapter::new(
            shared.path().to_path_buf(),
        ))],
    );
    scheduler.schedule_track(boot.track_id.clone()).await;
    assert_eq!(
        wait_for_terminal_row(&boot, "terminal-cwd", 30)
            .await
            .status,
        TaskStatus::Done
    );
}

#[tokio::test]
async fn long_task_explicit_gate_cwd_overrides_worker_checkout() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    let boot = boot().await;
    let explicit = tempfile::tempdir().unwrap();
    std::fs::write(explicit.path().join("explicit-evidence"), "ok").unwrap();
    let gate = json!({"cwd": explicit.path(), "steps": [{"name": "cwd", "cmd": "test -f explicit-evidence"}]}).to_string();
    let mut task = gate_task(&boot, "explicit-cwd", &gate);
    task.worker_card_id = Some(boot.worker_card_id.to_string());
    seed_task(&boot, task).await;
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(TaskVerifyAdapter::new(
            explicit.path().to_path_buf(),
        ))],
    );
    scheduler.schedule_track(boot.track_id.clone()).await;
    assert_eq!(
        wait_for_terminal_row(&boot, "explicit-cwd", 30)
            .await
            .status,
        TaskStatus::Done
    );
    let card = boot
        .repo
        .card_get(boot.worker_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        card.payload["gate_cwd"],
        explicit.path().to_str().unwrap(),
        "UI metadata must expose the gate override separately from worker cwd"
    );
}

#[tokio::test]
async fn long_task_terminal_report_ownership_and_outcome_matrix() {
    for (status, detail, foreign, success) in [
        (TaskStatus::Done, None, true, true),
        (TaskStatus::Failed, Some("worker-reported"), true, false),
        (TaskStatus::Failed, Some("gate-red"), false, true),
        (TaskStatus::Canceled, None, false, true),
        (TaskStatus::Done, None, false, false),
    ] {
        let boot = boot().await;
        set_lifecycle(&boot, TrackLifecycle::Working).await;
        let mut task = plan_task(&boot.track_id, "matrix", TaskKind::Codex, &[]);
        task.status = status;
        task.status_detail = detail.map(str::to_string);
        task.worker_card_id = Some(if foreign {
            "another-card".into()
        } else {
            boot.worker_card_id.to_string()
        });
        let task_id = task.id.clone();
        seed_task(&boot, task).await;
        let tool = if success {
            TOOL_TASK_COMPLETE
        } else {
            TOOL_TASK_FAIL
        };
        let result = call_tool(
            &boot,
            tool,
            worker_identity(&boot),
            json!({"idempotency_key": task_id, "result": {}, "reason": "late report"}),
        )
        .await;
        assert!(
            result.is_err(),
            "{status:?} {detail:?}, foreign={foreign}, success={success}"
        );
        assert!(event_rows(&boot, "task.completed").await.is_empty());
        assert!(event_rows(&boot, "task.failed").await.is_empty());
        assert_eq!(track_lifecycle(&boot).await, TrackLifecycle::Working);
    }
}

#[tokio::test]
async fn long_task_bound_worker_without_op_cannot_report_card_id() {
    let boot = boot().await;
    let mut task = plan_task(&boot.track_id, "bound", TaskKind::Codex, &[]);
    task.status = TaskStatus::Running;
    task.worker_card_id = Some(boot.worker_card_id.to_string());
    seed_task(&boot, task).await;
    let result = call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        worker_identity(&boot),
        json!({"idempotency_key": boot.worker_card_id, "result": {}}),
    )
    .await;
    assert!(
        result.is_err(),
        "the durable task-card binding also prevents the legacy key path"
    );
    assert!(event_rows(&boot, "task.completed").await.is_empty());
}
