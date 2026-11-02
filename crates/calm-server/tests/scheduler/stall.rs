//! Recovery must remain observable when worker startup capacity is exhausted.
use super::*;

#[tokio::test]
async fn steady_sweep_does_not_wait_for_exhausted_start_capacity() {
    let boot = boot().await;
    let mut task = plan_task(&boot.track_id, "held-start", TaskKind::Codex, &[]);
    task.status = TaskStatus::Pending;
    seed_projected_task(&boot, task).await;
    let capacity = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = capacity.clone().acquire_owned().await.unwrap();
    let (_runtime, scheduler) = build_scheduler_with_semaphore(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.as_str().to_string(),
        })],
        capacity,
    );

    let swept = tokio::time::timeout(Duration::from_secs(2), scheduler.sweep_all()).await;
    let status_while_held = task_row(&boot, "held-start").await.status;
    // Release before asserting so a red run leaves no blocked background work.
    drop(permit);
    assert!(
        swept.is_ok(),
        "steady reconciliation must not wait for startup capacity"
    );
    assert_eq!(status_while_held, TaskStatus::Pending);

    tokio::time::timeout(Duration::from_secs(5), async {
        while task_row(&boot, "held-start").await.status != TaskStatus::Running {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the original attempt still starts after capacity returns");
}

#[tokio::test]
async fn blocked_operation_does_not_hold_admission_or_track_lock() {
    let boot = boot().await;
    let mut first = plan_task(&boot.track_id, "first", TaskKind::Codex, &[]);
    first.access = calm_server::model::TaskAccess::ReadOnly;
    seed_projected_task(&boot, first).await;
    let capacity = Arc::new(tokio::sync::Semaphore::new(1));
    let (runtime, scheduler) = build_scheduler_with_semaphore(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: boot.worker_card_id.to_string(),
        })],
        capacity.clone(),
    );
    // A real operation drive fence, not a copy of scheduler behavior.
    let drive = runtime.lock_for_track_delete_for_test().await;
    scheduler.poke(boot.track_id.clone());
    wait_status(&boot, "first", TaskStatus::Dispatched).await;
    let available = tokio::time::timeout(Duration::from_secs(2), capacity.acquire()).await;
    let freed = available.is_ok();
    drop(available);
    let mut second = plan_task(&boot.track_id, "second", TaskKind::Codex, &[]);
    second.access = calm_server::model::TaskAccess::ReadOnly;
    seed_projected_task(&boot, second).await;
    scheduler.poke(boot.track_id.clone());
    let second_claimed = tokio::time::timeout(Duration::from_secs(2), async {
        while task_row(&boot, "second").await.status == TaskStatus::Pending {
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    drop(drive);
    wait_status(&boot, "first", TaskStatus::Running).await;
    wait_status(&boot, "second", TaskStatus::Running).await;
    assert!(
        freed,
        "operation driving must not consume Planner/admission capacity"
    );
    assert!(
        second_claimed,
        "operation driving must not hold the Track scheduling lock"
    );
    assert_eq!(operation_count(&boot, "codex-worker").await, 2);
}

#[tokio::test]
async fn stalled_verification_wakes_without_changing_execution() {
    let boot = boot().await;
    let mut task = plan_task(&boot.track_id, "unsettled-gate", TaskKind::Codex, &[]);
    task.status = TaskStatus::Verifying;
    task.updated_at_ms = now_ms() - 31 * 60 * 1000;
    task.gate_attempt = 1;
    seed_task(&boot, task.clone()).await;
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    scheduler.sweep_all().await;
    let wakes = event_rows(&boot, "track.wake_requested").await;
    assert_eq!(
        wakes.len(),
        1,
        "an unsettled task needs an actionable Planner observation"
    );
    assert_eq!(
        task_row(&boot, "unsettled-gate").await.status,
        TaskStatus::Verifying
    );
    assert!(
        task_row(&boot, "unsettled-gate")
            .await
            .gate_result_json
            .is_none()
    );
    // A new scheduler over the same persistence must not duplicate this episode.
    let (_runtime2, restarted) = build_scheduler(&boot, vec![]);
    restarted.sweep_all().await;
    assert_eq!(event_rows(&boot, "track.wake_requested").await.len(), 1);
}

fn notice_args(wake: &(String, Value), attempt: &str, decision: &str) -> Value {
    json!({"attempt_id":attempt,"notice_id":wake.1["key"],"decision":decision,
        "message":"Inspect execution evidence and record the next action"})
}

async fn stalled_fixture(
    status: TaskStatus,
) -> (Boot, Arc<OperationRuntime>, Arc<Scheduler>, Task) {
    let boot = boot().await;
    let mut task = plan_task(&boot.track_id, "stalled", TaskKind::Codex, &[]);
    task.status = status;
    task.updated_at_ms = now_ms() - 31 * 60 * 1000;
    seed_task(&boot, task.clone()).await;
    let (runtime, scheduler) =
        build_scheduler_with_semaphore(&boot, vec![], Arc::new(tokio::sync::Semaphore::new(0)));
    scheduler.sweep_all().await;
    (boot, runtime, scheduler, task)
}

#[tokio::test]
async fn stall_wait_replays_and_renotifies_after_deadline() {
    let (boot, _runtime, scheduler, task) = stalled_fixture(TaskStatus::Verifying).await;
    let wake = event_rows(&boot, "track.wake_requested").await.remove(0);
    let mut args = notice_args(&wake, &task.id, "wait");
    let invalid = call_tool(
        &boot,
        "neige_followup_settle",
        planner_identity(&boot),
        args.clone(),
    )
    .await
    .unwrap_err();
    assert_eq!(invalid.code, -32602);
    args["review_after_secs"] = json!(60);
    let receipt = call_tool(
        &boot,
        "neige_followup_settle",
        planner_identity(&boot),
        args.clone(),
    )
    .await
    .unwrap();
    assert!(receipt["review_at"].is_string());
    assert_eq!(
        call_tool(
            &boot,
            "neige_followup_settle",
            planner_identity(&boot),
            args.clone()
        )
        .await
        .unwrap(),
        receipt
    );
    scheduler.sweep_all().await;
    assert_eq!(event_rows(&boot, "track.wake_requested").await.len(), 1);
    boot.repo
        .sqlite_db()
        .unwrap()
        .writer()
        .execute(
            sqlx::query("UPDATE task_followups SET next_review_at_ms=0 WHERE task_id=?1")
                .bind(&task.id),
        )
        .await
        .unwrap();
    scheduler.sweep_all().await;
    assert_eq!(event_rows(&boot, "track.wake_requested").await.len(), 2);
    assert_eq!(
        call_tool(
            &boot,
            "neige_followup_settle",
            planner_identity(&boot),
            args
        )
        .await
        .unwrap_err()
        .code,
        -32409
    );
    assert_eq!(
        task_row(&boot, "stalled").await.status,
        TaskStatus::Verifying
    );
}

#[tokio::test]
async fn stall_bypass_preserves_gate_and_refuses_unsupported_termination() {
    let (boot, _runtime, _scheduler, task) = stalled_fixture(TaskStatus::Verifying).await;
    let wake = event_rows(&boot, "track.wake_requested").await.remove(0);
    assert_eq!(
        call_tool(
            &boot,
            "neige_followup_settle",
            planner_identity(&boot),
            notice_args(&wake, &task.id, "terminate")
        )
        .await
        .unwrap_err()
        .code,
        -32409
    );
    call_tool(
        &boot,
        "neige_followup_settle",
        planner_identity(&boot),
        notice_args(&wake, &task.id, "bypass"),
    )
    .await
    .unwrap();
    assert_eq!(
        task_row(&boot, "stalled").await.status,
        TaskStatus::Verifying
    );
    assert!(task_row(&boot, "stalled").await.gate_result_json.is_none());
    assert!(event_rows(&boot, "task.completed").await.is_empty());
    assert!(event_rows(&boot, "task.failed").await.is_empty());
    assert!(event_rows(&boot, "workspace.released").await.is_empty());
}

#[tokio::test]
async fn stall_terminate_cancels_only_the_observed_pending_attempt() {
    let (boot, _runtime, _scheduler, task) = stalled_fixture(TaskStatus::Pending).await;
    let wake = event_rows(&boot, "track.wake_requested").await.remove(0);
    let args = notice_args(&wake, &task.id, "terminate");
    assert_eq!(
        call_tool(
            &boot,
            "neige_followup_settle",
            worker_identity(&boot),
            args.clone()
        )
        .await
        .unwrap_err()
        .code,
        -32403
    );
    let receipt = call_tool(
        &boot,
        "neige_followup_settle",
        planner_identity(&boot),
        args.clone(),
    )
    .await
    .unwrap();
    assert!(receipt["review_at"].is_null());
    assert_eq!(
        task_row(&boot, "stalled").await.status,
        TaskStatus::Canceled
    );
    assert_eq!(event_rows(&boot, "plan.updated").await.len(), 1);
    assert_eq!(
        call_tool(
            &boot,
            "neige_followup_settle",
            planner_identity(&boot),
            args
        )
        .await
        .unwrap(),
        receipt
    );
    assert_eq!(event_rows(&boot, "plan.updated").await.len(), 1);
}

#[tokio::test]
async fn stall_notice_cannot_act_after_phase_change_or_track_close() {
    let (boot, _runtime, scheduler, task) = stalled_fixture(TaskStatus::Pending).await;
    let wake = event_rows(&boot, "track.wake_requested").await.remove(0);
    let args = notice_args(&wake, &task.id, "bypass");
    boot.repo
        .sqlite_db()
        .unwrap()
        .writer()
        .execute(sqlx::query("UPDATE tasks SET status='dispatched' WHERE id=?1").bind(&task.id))
        .await
        .unwrap();
    assert_eq!(
        call_tool(
            &boot,
            "neige_followup_settle",
            planner_identity(&boot),
            args.clone()
        )
        .await
        .unwrap_err()
        .code,
        -32409
    );
    set_closed(&boot, true).await;
    scheduler.sweep_all().await;
    assert_eq!(event_rows(&boot, "track.wake_requested").await.len(), 1);
    assert_eq!(
        call_tool(
            &boot,
            "neige_followup_settle",
            planner_identity(&boot),
            args
        )
        .await
        .unwrap_err()
        .code,
        -32409
    );
}

#[tokio::test]
async fn stall_followup_refuses_a_foreign_track_attempt() {
    let boot = boot().await;
    let foreign = boot
        .repo
        .track_create(NewTrack {
            area_id: boot.area_id.clone(),
            title: "foreign".into(),
            sort: None,
            cwd: "/foreign".into(),
            template_id: None,
            template_input: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let mut task = plan_task(&foreign.id, "foreign-stall", TaskKind::Codex, &[]);
    task.status = TaskStatus::Verifying;
    task.updated_at_ms = now_ms() - 31 * 60 * 1000;
    seed_task(&boot, task.clone()).await;
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    scheduler.sweep_all().await;
    let wake = event_rows(&boot, "track.wake_requested").await.remove(0);
    assert_eq!(
        call_tool(
            &boot,
            "neige_followup_settle",
            planner_identity(&boot),
            notice_args(&wake, &task.id, "bypass")
        )
        .await
        .unwrap_err()
        .code,
        -32409
    );
    assert_eq!(
        boot.repo.task_get(&task.id).await.unwrap().unwrap().status,
        TaskStatus::Verifying
    );
}

#[tokio::test]
async fn stalled_running_terminal_requires_a_decision_without_fabricating_exit() {
    let boot = boot().await;
    let mut task = plan_task(
        &boot.track_id,
        "running-terminal-stall",
        TaskKind::Terminal,
        &[],
    );
    task.status = TaskStatus::Running;
    task.updated_at_ms = now_ms() - 31 * 60 * 1000;
    seed_task(&boot, task.clone()).await;
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);

    scheduler.sweep_all().await;
    let wakes = event_rows(&boot, "track.wake_requested").await;
    assert_eq!(
        wakes.len(),
        1,
        "running terminal without settlement needs Planner attention"
    );
    let mut args = notice_args(&wakes[0], &task.id, "wait");
    args["review_after_secs"] = json!(60);
    call_tool(
        &boot,
        "neige_followup_settle",
        planner_identity(&boot),
        args,
    )
    .await
    .unwrap();
    assert_eq!(
        task_row(&boot, "running-terminal-stall").await.status,
        TaskStatus::Running
    );
    assert!(event_rows(&boot, "task.completed").await.is_empty());
    assert!(event_rows(&boot, "task.failed").await.is_empty());
    assert!(event_rows(&boot, "workspace.released").await.is_empty());
}

#[tokio::test]
async fn stall_followup_refuses_retired_planner_session() {
    let (boot, _runtime, _scheduler, task) = stalled_fixture(TaskStatus::Pending).await;
    let wake = event_rows(&boot, "track.wake_requested").await.remove(0);
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), |tx| {
        Box::pin(async move {
            calm_server::db::sqlite::session_supersede_active_tx(
                tx,
                &"planner-session".to_string(),
                now_ms(),
            )
            .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    crate::support::planner::start_session(
        boot.repo.as_ref(),
        boot.planner_card_id.as_str(),
        "replacement-planner",
    )
    .await;
    for decision in ["bypass", "wait", "terminate"] {
        let mut args = notice_args(&wake, &task.id, decision);
        if decision == "wait" {
            args["review_after_secs"] = json!(60);
        }
        assert_eq!(
            call_tool(
                &boot,
                "neige_followup_settle",
                planner_identity(&boot),
                args
            )
            .await
            .unwrap_err()
            .code,
            -32403
        );
    }
    assert_eq!(task_row(&boot, "stalled").await.status, TaskStatus::Pending);
    assert!(event_rows(&boot, "plan.updated").await.is_empty());
    let args = notice_args(&wake, &task.id, "terminate");
    let mut current = planner_identity(&boot);
    current.session_id = "replacement-planner".into();
    call_tool(&boot, "neige_followup_settle", current, args)
        .await
        .unwrap();
    assert_eq!(
        task_row(&boot, "stalled").await.status,
        TaskStatus::Canceled
    );
}

#[tokio::test]
async fn stall_decision_replay_rejects_a_superseded_attempt() {
    use calm_types::task_recovery::{
        TASK_IN_TRACK_ROUTE, TaskAttemptOrigin, TaskRecoveryConstraint,
    };
    let (boot, _runtime, _scheduler, task) = stalled_fixture(TaskStatus::Verifying).await;
    let wake = event_rows(&boot, "track.wake_requested").await.remove(0);
    let args = notice_args(&wake, &task.id, "bypass");
    call_tool(
        &boot,
        "neige_followup_settle",
        planner_identity(&boot),
        args.clone(),
    )
    .await
    .unwrap();

    // A released recovery allocation supersedes the observed attempt. Replay of an
    // old decision must not acknowledge the successor's outstanding work.
    let db = boot.repo.sqlite_db().unwrap();
    db.writer()
        .execute(sqlx::query("UPDATE tasks SET status='failed' WHERE id=?1").bind(&task.id))
        .await
        .unwrap();
    let successor_id = format!("{}:successor", task.id);
    let origin = TaskAttemptOrigin::Recovery {
        previous_attempt_id: task.id.clone(),
        idempotency_key: "followup-successor".into(),
        request_fingerprint: "fixture".into(),
        reason: "historical recovery".into(),
        actor: calm_server::ids::ActorId::User,
        constraint: TaskRecoveryConstraint::V1 {
            refs: vec![calm_types::event::TaskContextRef {
                track_id: boot.track_id.clone(),
                block_id: "stall-root".into(),
                rev: 1,
                hash: "0".repeat(64),
                is_root: true,
            }],
            spawn: TASK_IN_TRACK_ROUTE.into(),
            declared_by: "user".into(),
        },
    };
    db.writer()
        .execute(
            sqlx::query(
                "INSERT INTO task_attempt_allocations(
            attempt_id,track_id,key,generation,origin_json,created_at_ms
         ) VALUES(?1,?2,?3,2,?4,0)",
            )
            .bind(&successor_id)
            .bind(&task.track_id)
            .bind(&task.key)
            .bind(serde_json::to_string(&origin).unwrap()),
        )
        .await
        .unwrap();
    let mut successor = task.clone();
    successor.id = successor_id.clone();
    successor.updated_at_ms = now_ms();
    seed_task(&boot, successor).await;
    assert_eq!(
        call_tool(
            &boot,
            "neige_followup_settle",
            planner_identity(&boot),
            args
        )
        .await
        .unwrap_err()
        .code,
        -32409
    );
    assert_eq!(
        boot.repo
            .task_get(&successor_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Verifying
    );
    assert!(event_rows(&boot, "task.completed").await.is_empty());
    assert!(event_rows(&boot, "workspace.released").await.is_empty());
}
