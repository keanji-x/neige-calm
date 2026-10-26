//! Continue a handed-back child through declaration, operation and scheduler entry points.
use super::*;

#[tokio::test]
async fn child_track_continue_reuses_child_and_resumes_claim_rejects_stale_attempt() {
    let (boot, _repository) = child_track_config::boot_git_parent().await;
    let (runtime, scheduler) = build_scheduler(&boot, vec![child_adapter(&boot)]);
    let mut first = plan_task(&boot.track_id, "first-round", TaskKind::Codex, &[]);
    first.spawn = "sub-track".into();
    first.status = TaskStatus::Dispatched;
    let old = first.id.clone();
    seed_projected_task(&boot, first.clone()).await;
    let input = build_child_track_payload(&first).unwrap();
    let operation = runtime
        .submit(
            "child-track",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(old.clone()),
                payload_hash: stable_payload_hash(&input).unwrap(),
            },
            input,
        )
        .await
        .unwrap();
    let created = match runtime.wait(&operation).await.unwrap().outcome {
        OperationOutcome::Succeeded { result } => result,
        other => panic!("{other:?}"),
    };
    let child = created["child_track_id"].as_str().unwrap().to_owned();
    let planner = created["planner_card_id"].as_str().unwrap().to_owned();
    let db = boot.repo.sqlite_db().unwrap();
    support::planner::start_session(boot.repo.as_ref(), &planner, "child-session").await;
    let identity = ToolCallIdentity {
        card_id: planner.clone(),
        session_id: "child-session".into(),
        track_id: Some(child.clone()),
        ..planner_identity(&boot)
    };
    support::report_writes::upsert_block(
        &boot.ctx,
        &boot.registry,
        identity.clone(),
        json!({"kind":"prose","markdown":"Retained findings from the first round"}),
    )
    .await
    .unwrap();
    call_tool(
        &boot,
        "neige_task_fail",
        identity.clone(),
        json!({"attempt_id":old,"reason":"new direction needed"}),
    )
    .await
    .unwrap();
    seed_child_task(&boot, &child, "left", TaskStatus::Pending).await;
    let before = boot.repo.track_get(&child).await.unwrap().unwrap();
    let cards = boot.repo.cards_by_track(&child).await.unwrap();
    let sessions_before: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id,thread_id FROM worker_sessions WHERE card_id=?1")
            .bind(&planner)
            .fetch_all(db.read_pool())
            .await
            .unwrap();
    let checkout = before.workspace.agent_cwd();
    let marker = std::path::Path::new(checkout).join("continued-uncommitted.txt");
    std::fs::write(&marker, "retained child work").unwrap();
    let head_before = git_head(checkout);
    // A changed parent HEAD must never become the continued child's base.
    let commit = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "parent moved",
        ])
        .current_dir(_repository.path())
        .output()
        .unwrap();
    assert!(commit.status.success());
    let mut task = plan_task(&boot.track_id, "second-round", TaskKind::Codex, &[]);
    task.spawn = "sub-track".into();
    task.goal = "new assignment".into();
    task.child_track_json = Some(json!({"continue":child}).to_string());
    let id = task.id.clone();
    seed_projected_task(&boot, task).await;
    scheduler.schedule_track(boot.track_id.clone()).await;
    let task = boot.repo.task_get(&id).await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Running, "{:?}", task.status_detail);
    let bound: String = sqlx::query_scalar("SELECT child_track_id FROM tasks WHERE id=?1")
        .bind(&id)
        .fetch_one(db.read_pool())
        .await
        .unwrap();
    assert_eq!(bound, child);
    assert_eq!(
        boot.repo
            .track_get(&child)
            .await
            .unwrap()
            .unwrap()
            .workspace,
        before.workspace
    );
    assert_eq!(
        serde_json::to_value(boot.repo.cards_by_track(&child).await.unwrap()).unwrap(),
        serde_json::to_value(cards).unwrap()
    );
    assert_eq!(operation_count(&boot, "planner-harness-start").await, 0);
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        "retained child work"
    );
    assert_eq!(git_head(checkout), head_before);
    let sessions_after: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id,thread_id FROM worker_sessions WHERE card_id=?1")
            .bind(&planner)
            .fetch_all(db.read_pool())
            .await
            .unwrap();
    assert_eq!(sessions_after, sessions_before);
    let replay_input = build_child_track_payload(&task).unwrap();
    let replay = runtime
        .submit(
            "child-track",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(id.clone()),
                payload_hash: stable_payload_hash(&replay_input).unwrap(),
            },
            replay_input,
        )
        .await
        .unwrap();
    assert!(matches!(
        runtime.wait(&replay).await.unwrap().outcome,
        OperationOutcome::Succeeded { .. } | OperationOutcome::SucceededViaCollision { .. }
    ));
    let wakes = event_rows(&boot, "track.wake_requested").await;
    assert_eq!(wakes.len(), 1);
    assert_eq!(wakes[0].1["source"], "parent_task");
    assert_eq!(wakes[0].1["key"], id);
    assert_eq!(wakes[0].1["track_id"], child);
    assert!(
        wakes[0].1["text"]
            .as_str()
            .unwrap()
            .contains("task key: second-round")
    );
    assert!(
        wakes[0].1["text"]
            .as_str()
            .unwrap()
            .contains("new assignment")
    );
    let mut tx = db.writer_pool().begin().await.unwrap();
    assert_eq!(
        calm_truth::db::sqlite::task_claim_pending_tx(
            &mut tx,
            &format!("{child}:left"),
            now_ms(),
            &[],
            false
        )
        .await
        .unwrap(),
        1
    );
    tx.rollback().await.unwrap();
    sqlx::query("UPDATE tasks SET status='done' WHERE id=?1")
        .bind(format!("{child}:left"))
        .execute(db.writer_pool())
        .await
        .unwrap();
    for tool in ["neige_task_done", "neige_task_fail"] {
        let mut args = json!({"attempt_id":old});
        if tool == "neige_task_fail" {
            args["reason"] = json!("stale seed");
        }
        assert!(
            call_tool(&boot, tool, identity.clone(), args)
                .await
                .is_err(),
            "{tool} must reject the old seed"
        );
        assert_eq!(
            boot.repo.task_get(&id).await.unwrap().unwrap().status,
            TaskStatus::Running
        );
        assert!(
            boot.repo
                .track_get(&child)
                .await
                .unwrap()
                .unwrap()
                .closed_at
                .is_none()
        );
    }
    call_tool(&boot, "neige_task_done", identity, json!({"attempt_id":id}))
        .await
        .unwrap();
    scheduler
        .reconcile_child_track_for_test(&child)
        .await
        .unwrap();
    assert_eq!(
        boot.repo.task_get(&id).await.unwrap().unwrap().status,
        TaskStatus::Done
    );
    assert_eq!(
        boot.repo.task_get(&old).await.unwrap().unwrap().status,
        TaskStatus::Failed
    );
}

fn child_adapter(boot: &Boot) -> Arc<dyn ProviderAdapter> {
    Arc::new(ChildTrackAdapter::new(
        Arc::new(calm_server::acp_planner::config::AcpPlannerHost::unconfigured_scratch().unwrap()),
        boot.card_role_cache.clone(),
        boot.track_area_cache.clone(),
        child_track_workspace_root(),
        std::path::PathBuf::from("/unused-objective-checkouts"),
        workspace_runtime::sweep::TeardownTargets::default(),
    ))
}

fn git_head(cwd: &str) -> String {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test]
async fn child_track_continue_stamp_rejects_second_active_binding() {
    for status in ["dispatched", "running"] {
        let boot = boot().await;
        let (old, child) = seed_child_parent(&boot, "occupied", false, None).await;
        let db = boot.repo.sqlite_db().unwrap();
        sqlx::query("UPDATE tasks SET status=?1 WHERE id=?2")
            .bind(status)
            .bind(old)
            .execute(db.writer_pool())
            .await
            .unwrap();
        let mut task = plan_task(&boot.track_id, "new-key", TaskKind::Codex, &[]);
        task.spawn = "sub-track".into();
        task.status = TaskStatus::Dispatched;
        let id = task.id.clone();
        seed_task(&boot, task).await;
        let mut tx = db.writer_pool().begin().await.unwrap();
        let error = calm_truth::db::sqlite::task_bind_child_track_tx(
            &mut tx,
            &id,
            boot.track_id.as_str(),
            &child,
            now_ms(),
        )
        .await
        .expect_err("the write fence itself must refuse a second active binding");
        assert!(matches!(
            error,
            calm_truth::error::TruthError::Core(calm_types::error::CoreError::Conflict(_))
        ));
        let bound: Option<String> =
            sqlx::query_scalar("SELECT child_track_id FROM tasks WHERE id=?1")
                .bind(&id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(bound, None);
        tx.rollback().await.unwrap();
    }
}

#[tokio::test]
async fn child_track_continue_rejects_dynamic_target_changes_without_effects() {
    for change in [
        "active",
        "foreign",
        "closed",
        "not-handed-back",
        "child-busy",
        "same-key",
        "event-failure",
    ] {
        let boot = boot().await;
        let (old, child) = seed_child_parent(&boot, "old-key", false, None).await;
        let db = boot.repo.sqlite_db().unwrap();
        sqlx::query(
            "UPDATE tasks SET status='failed',status_detail='child-track-handed-back' WHERE id=?1",
        )
        .bind(&old)
        .execute(db.writer_pool())
        .await
        .unwrap();
        let mut task = plan_task(&boot.track_id, "fresh-key", TaskKind::Codex, &[]);
        task.spawn = "sub-track".into();
        task.status = TaskStatus::Dispatched;
        task.child_track_json = Some(json!({"continue":child}).to_string());
        let id = task.id.clone();
        seed_task(&boot, task.clone()).await;
        match change {
            "active" => {
                // A valid handback still exists; this competing binding must be stopped by stamp SQL.
                let mut competing = plan_task(&boot.track_id, "competitor", TaskKind::Codex, &[]);
                competing.spawn = "sub-track".into();
                competing.status = TaskStatus::Running;
                let competing_id = competing.id.clone();
                seed_task(&boot, competing).await;
                sqlx::query("UPDATE tasks SET child_track_id=?1 WHERE id=?2")
                    .bind(&child)
                    .bind(competing_id)
                    .execute(db.writer_pool())
                    .await
                    .unwrap();
            }
            "foreign" => {
                sqlx::query("INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) SELECT 'foreign-parent',\
                    area_id,'other parent',2,1,1 FROM tracks WHERE id=?1")
                    .bind(boot.track_id.as_str()).execute(db.writer_pool()).await.unwrap();
                sqlx::query("UPDATE tracks SET parent_track_id='foreign-parent' WHERE id=?1")
                    .bind(&child)
                    .execute(db.writer_pool())
                    .await
                    .unwrap();
            }
            "closed" => {
                sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1")
                    .bind(&child)
                    .execute(db.writer_pool())
                    .await
                    .unwrap();
            }
            "not-handed-back" => {
                sqlx::query("UPDATE tasks SET status_detail='other-failure' WHERE id=?1")
                    .bind(&old)
                    .execute(db.writer_pool())
                    .await
                    .unwrap();
            }
            "child-busy" => {
                seed_child_task(&boot, &child, "busy", TaskStatus::Running).await;
            }
            "same-key" => {
                // The old task identity carries its original key. Do not rewrite immutable
                // allocation identity merely to build this refused continuation request.
                task.id = old.clone();
            }
            "event-failure" => {
                sqlx::query("CREATE TRIGGER refuse_continue_wake BEFORE INSERT ON events WHEN \
                    NEW.kind='track.wake_requested' BEGIN SELECT RAISE(ABORT,'wake injection'); END")
                .execute(db.writer_pool()).await.unwrap();
            }
            _ => unreachable!(),
        }
        let input = build_child_track_payload(&task).unwrap();
        let repo = SqlxOperationRepo::new(db.clone());
        let op_id = repo
            .insert_operation(
                "child-track",
                OperationKey {
                    operation_key: new_id(),
                    idempotency_key: Some(id.clone()),
                    payload_hash: stable_payload_hash(&input).unwrap(),
                },
                input.clone(),
            )
            .await
            .unwrap();
        let op = repo.get_operation(&op_id).await.unwrap().unwrap();
        let before = resource_counts(&boot).await;
        let mut tx = db.writer_pool().begin().await.unwrap();
        let error = child_adapter(&boot)
            .prepare_tx(&mut tx, &input, &op)
            .await
            .expect_err(change);
        if change != "event-failure" {
            assert!(
                matches!(error, calm_server::error::CalmError::Conflict(_)),
                "{change}: {error}"
            );
        }
        tx.rollback().await.unwrap();
        assert_eq!(resource_counts(&boot).await, before, "{change}");
        let bound: Option<String> =
            sqlx::query_scalar("SELECT child_track_id FROM tasks WHERE id=?1")
                .bind(id)
                .fetch_one(db.read_pool())
                .await
                .unwrap();
        assert_eq!(bound, None, "{change}");
    }
}

async fn resource_counts(boot: &Boot) -> (i64, i64, i64, i64) {
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM tracks),(SELECT count(*) FROM cards),\
        (SELECT count(*) FROM worker_sessions),(SELECT count(*) FROM events)",
    )
    .fetch_one(boot.repo.sqlite_db().unwrap().read_pool())
    .await
    .unwrap()
}
