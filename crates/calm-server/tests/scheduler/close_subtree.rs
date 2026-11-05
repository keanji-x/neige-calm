use super::*;

// The shared scheduler fixture reserves a second live worker for unrelated worker tests.
// These close tests create their own work; end the unused carrier before testing that scope.
async fn finish_unused_fixture_worker(boot: &Boot) {
    boot.repo
        .sqlite_db()
        .unwrap()
        .writer()
        .execute(
            sqlx::query(
                "UPDATE worker_sessions SET state='exited',completed_at_ms=2 WHERE card_id=?1",
            )
            .bind(boot.worker_card_id.as_str()),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn close_closes_descendants_serializes_child_creation_and_canceled_spawn_replay() {
    child_close_race(false).await;
}

#[tokio::test]
async fn close_closes_descendants_serializes_child_continue_and_canceled_spawn_replay() {
    child_close_race(true).await;
}

async fn child_close_race(continuation: bool) {
    for order in ["close-first", "create-first", "concurrent"] {
        let (boot, _repository) = child_track_config::boot_git_admin_parent().await;
        finish_unused_fixture_worker(&boot).await;
        let (runtime, scheduler) =
            build_scheduler(&boot, vec![child_track_continue::child_adapter(&boot)]);
        let returned = if continuation {
            let mut first = plan_task(&boot.track_id, "first", TaskKind::Codex, &[]);
            first.spawn = "sub-track".into();
            first.status = TaskStatus::Dispatched;
            seed_projected_task(&boot, first.clone()).await;
            let input = build_child_track_payload(&first).unwrap();
            let op = runtime
                .submit(
                    "child-track",
                    OperationKey {
                        operation_key: new_id(),
                        idempotency_key: Some(first.id.clone()),
                        payload_hash: stable_payload_hash(&input).unwrap(),
                    },
                    input,
                )
                .await
                .unwrap();
            let created = match runtime.wait(&op).await.unwrap().outcome {
                OperationOutcome::Succeeded { result } => result,
                other => panic!("{other:?}"),
            };
            let child = created["child_track_id"].as_str().unwrap().to_owned();
            call_tool(
                &boot,
                "neige_task_fail",
                ToolCallIdentity {
                    card_id: created["planner_card_id"].as_str().unwrap().into(),
                    session_id: created["canonical_worker"].as_str().unwrap().into(),
                    track_id: Some(child.clone()),
                    ..planner_identity(&boot)
                },
                json!({"attempt_id":first.id,"reason":"continue in another round"}),
            )
            .await
            .unwrap();
            Some(child)
        } else {
            None
        };
        let mut task = plan_task(&boot.track_id, "child-race", TaskKind::Codex, &[]);
        task.spawn = "sub-track".into();
        task.status = TaskStatus::Dispatched;
        task.child_track_json = returned.map(|child| json!({"continue":child}).to_string());
        let attempt = task.id.clone();
        seed_projected_task(&boot, task.clone()).await;
        let input = build_child_track_payload(&task).unwrap();
        let create = || async {
            let operation = runtime
                .submit(
                    "child-track",
                    OperationKey {
                        operation_key: new_id(),
                        idempotency_key: Some(attempt.clone()),
                        payload_hash: stable_payload_hash(&input).unwrap(),
                    },
                    input.clone(),
                )
                .await
                .unwrap();
            runtime.wait(&operation).await.unwrap().outcome
        };
        let close = || {
            call_tool(
                &boot,
                "neige_track_close",
                planner_identity(&boot),
                json!({"message":"stop"}),
            )
        };
        match order {
            "close-first" => {
                close().await.unwrap();
                let outcome = create().await;
                assert!(
                    matches!(
                        outcome,
                        OperationOutcome::Failed { .. } | OperationOutcome::Stuck { .. }
                    ),
                    "{outcome:?}"
                );
            }
            "create-first" => {
                let outcome = create().await;
                assert!(
                    matches!(outcome, OperationOutcome::Succeeded { .. }),
                    "{outcome:?}"
                );
                let count: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM tracks WHERE parent_track_id=?1 AND closed_at IS NULL",
                )
                .bind(boot.track_id.as_str())
                .fetch_one(boot.repo.sqlite_db().unwrap().read_pool())
                .await
                .unwrap();
                assert_eq!(count, 1);
                close().await.unwrap();
            }
            "concurrent" => {
                let (a, _) = tokio::join!(close(), create());
                a.unwrap();
            }
            _ => unreachable!(),
        }
        assert_eq!(
            boot.repo.task_get(&attempt).await.unwrap().unwrap().status,
            TaskStatus::Canceled,
            "{order}"
        );
        let db = boot.repo.sqlite_db().unwrap();
        let open_children: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM tracks WHERE parent_track_id=?1 AND closed_at IS NULL",
        )
        .bind(boot.track_id.as_str())
        .fetch_one(db.read_pool())
        .await
        .unwrap();
        assert_eq!(open_children, 0, "{order}");
        // Reopen cannot make a delayed original operation derive a new child or revive its task.
        db.writer()
            .execute(
                sqlx::query("UPDATE tracks SET closed_at=NULL WHERE id=?1")
                    .bind(boot.track_id.as_str()),
            )
            .await
            .unwrap();
        scheduler.sweep_all().await;
        assert_eq!(
            boot.repo.task_get(&attempt).await.unwrap().unwrap().status,
            TaskStatus::Canceled
        );
        let open_children: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM tracks WHERE parent_track_id=?1 AND closed_at IS NULL",
        )
        .bind(boot.track_id.as_str())
        .fetch_one(db.read_pool())
        .await
        .unwrap();
        assert_eq!(open_children, 0);
    }
}

#[tokio::test]
async fn close_cancels_running_child_assignment_gate_through_its_execution_owner() {
    let _guard = GATE_SPAWN_TEST_LOCK.lock().await;
    let boot = boot().await;
    finish_unused_fixture_worker(&boot).await;
    let dir = unique_gate_dir("close-child-gate");
    let gate = json!({"cwd":dir.to_str().unwrap(),"steps":[{"name":"held","cmd":"touch started; sleep 60"}]}).to_string();
    let (_attempt, child) = seed_child_parent(&boot, "close-gate", true, Some(gate)).await;
    let (runtime, scheduler) =
        build_scheduler(&boot, vec![Arc::new(TaskVerifyAdapter::new(dir.clone()))]);
    assert!(boot.ctx.operation_runtime.set(runtime.clone()).is_ok());
    scheduler
        .reconcile_child_track_for_test(&child)
        .await
        .unwrap();
    let drive = {
        let scheduler = scheduler.clone();
        let track = boot.track_id.clone();
        tokio::spawn(async move {
            scheduler.schedule_track(track).await;
        })
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        while !dir.join("started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    call_tool(
        &boot,
        "neige_track_close",
        planner_identity(&boot),
        json!({"message":"stop gate and subtree"}),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(15), drive)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        task_row(&boot, "close-gate").await.status,
        TaskStatus::Canceled
    );
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let unsettled: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM workspace_executions WHERE settled_at_ms IS NULL",
            )
            .fetch_one(boot.repo.sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
            if unsettled == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(event_rows(&boot, "task.gate_result").await.is_empty());
    scheduler.sweep_all().await;
    assert_eq!(
        task_row(&boot, "close-gate").await.status,
        TaskStatus::Canceled
    );
    std::fs::remove_dir_all(dir).unwrap();
}
