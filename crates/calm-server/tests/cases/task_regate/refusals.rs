//! #2405 T3 and T7: what `neige_task_regate` refuses, and that a refusal writes nothing.
use super::*;
use calm_server::model::NewTrack;

// ---------------------------------------------------------------------------
// T3: what a regate refuses, and that a refusal writes nothing.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn regate_refused_while_checkout_busy() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let (task, _, _) = red(&fx, "busy-a", gate_on(&outside(&fx, "green"), None)).await;
    let other = fx.new_worker("busy-b", AgentProvider::Codex).await;
    fx.kernel_lease(&other.card_id).await;
    fx.running_task("busy-b", "codex", &other.card_id, json!({}))
        .await;

    refused(&fx, &task.id, -32409, "the track checkout is in use").await;
    assert_eq!(current(&fx.boot, "busy-a").await.status, TaskStatus::Failed);
    assert!(regate_events(&fx).await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn regate_refused_while_a_gate_op_is_unfinished() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let (task, lease, _) =
        settled(&fx, "stuck", gate_on(&outside(&fx, "g"), None), Some("x\n")).await;
    // Nothing can be sampled: prepare fails, the op is `stuck` and the row fails `gate-infra`.
    std::fs::write(lease.path.join(".git"), "garbage\n").unwrap();
    drive(&fx, "stuck").await;
    assert_eq!(
        gate_ops(&fx, &task.id).await,
        vec![("1".into(), "stuck".into())]
    );
    let row = current(&fx.boot, "stuck").await;
    assert_eq!(row.status_detail.as_deref(), Some("gate-infra"), "{row:?}");

    refused(
        &fx,
        &task.id,
        -32409,
        "a gate op of this task is unfinished or stuck (attempt 1 is stuck); the kernel cannot prove no gate process is running",
    )
    .await;
    assert_eq!(current(&fx.boot, "stuck").await.status, TaskStatus::Failed);
}

/// Even a terminal operation phase cannot authorize regate while its Engine scope is unsettled.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn regate_refused_while_a_previous_gate_process_survives() {
    use calm_server::operation::PhaseTag;
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let green = outside(&fx, "green");
    let release = outside(&fx, "release-survivor");
    let (task, _, _) = red(&fx, "survivor", gate_on(&green, None)).await;
    let previous_verdict = current(&fx.boot, "survivor").await.gate_result_json;
    std::fs::write(&green, b"").unwrap();
    sqlx::query("UPDATE tasks SET gate_json=?1 WHERE id=?2")
        .bind(gate_on(&green, Some(&release))["gate"].to_string())
        .bind(&task.id)
        .execute(&fx.pool())
        .await
        .unwrap();
    regate(&fx, &task.id).await.unwrap();
    let events_before = regate_events(&fx).await.len();
    let retry = current(&fx.boot, "survivor").await;
    let scheduler = fx.scheduler();
    let drive = tokio::spawn(async move { scheduler.drive_gate_for_test(retry).await });
    let op = tokio::time::timeout(WAIT, async {
        loop {
            if let Some(op) = fx
                .runtime
                .find_by_kind_and_idempotency(TASK_VERIFY_KIND, &format!("{}#g3", task.id))
                .await
                .unwrap()
                && op.phase.tag() == PhaseTag::Parked
                && op.tx_output.as_ref().unwrap().data["gate_execution"]["release"]
                    .get("Released")
                    .is_some()
            {
                break op;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    // Model a terminal op row with a still-running execution, as the occupancy reader must
    // defend against. The real executor, rather than PID artifacts, supplies the missing proof.
    sqlx::query("UPDATE operations SET phase='succeeded' WHERE id=?1")
        .bind(&op.id)
        .execute(&fx.pool())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE tasks SET status='failed',status_detail='gate-red',gate_result_json=?2 WHERE id=?1",
    )
    .bind(&task.id)
    .bind(previous_verdict)
    .execute(&fx.pool())
    .await
    .unwrap();
    let result = regate(&fx, &task.id).await;
    std::fs::write(&release, b"").unwrap();
    let id = serde_json::from_value(
        op.tx_output.as_ref().unwrap().data["gate_execution"]["execution_id"].clone(),
    )
    .unwrap();
    tokio::time::timeout(
        WAIT,
        fx.execution.lifecycle.engine().unwrap().wait_settled(&id),
    )
    .await
    .unwrap()
    .unwrap();
    // The synthetic terminal row has no completion result for the scheduler to reconcile.
    let _ = drive.await.unwrap();
    let err = result.expect_err("a surviving gate process refuses the re-run");
    assert_eq!(err.code, -32409, "{err:?}");
    assert!(
        err.message
            .contains("the previous gate's processes (attempt 3) could not be proven stopped"),
        "{err:?}"
    );
    assert_eq!(
        current(&fx.boot, "survivor").await.status,
        TaskStatus::Failed
    );
    assert_eq!(regate_events(&fx).await.len(), events_before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn regate_refuses_rows_that_cannot_rerun() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();

    // Not an attempt of this Track: unknown, or another Track's.
    let other = fx
        .boot
        .repo
        .track_create(NewTrack {
            template_input: None,
            area_id: fx.boot.area_id.clone(),
            title: "other".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    sqlx::query(concat!(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,status_detail,gate_json,",
        "created_at_ms,updated_at_ms) VALUES('foreign-1',?1,'foreign','codex','goal','{}',",
        "'failed','gate-red','{\"steps\":[]}',1,1)"
    ))
    .bind(other.id.as_str())
    .execute(&fx.pool())
    .await
    .unwrap();
    for attempt in ["foreign-1", "no-such-attempt"] {
        refused(&fx, attempt, -32404, "is not a task attempt of this track").await;
    }

    // A passed gate.
    let (passed, _, _) = settled(
        &fx,
        "passed",
        gate_on(&outside(&fx, "always"), None),
        Some("p\n"),
    )
    .await;
    std::fs::write(outside(&fx, "always"), b"").unwrap();
    drive(&fx, "passed").await;
    refused(&fx, &passed.id, -32409, "task passed is done").await;

    // A failure the worker reported: its gate never judged the candidate.
    let worker = fx.new_worker("reported", AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&worker.card_id).await;
    let reported = fx
        .running_task(
            "reported",
            "codex",
            &worker.card_id,
            gate_on(&outside(&fx, "always"), None),
        )
        .await;
    std::fs::write(lease.path.join("reported.txt"), "partial\n").unwrap();
    call_tool(
        &fx.boot,
        "neige_task_fail",
        worker.clone(),
        json!({"attempt_id": reported.id, "reason": "could not finish"}),
    )
    .await
    .unwrap();
    settle_by_hand(&fx, &reported.id).await;
    refused(
        &fx,
        &reported.id,
        -32409,
        "failed before its gate judged a candidate",
    )
    .await;

    // A failed delivery: no candidate was ever gated.
    let worker = fx.new_worker("undelivered", AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&worker.card_id).await;
    install_pre_commit(&lease, HOOK_EXIT_1);
    let undelivered = fx
        .running_task(
            "undelivered",
            "codex",
            &worker.card_id,
            gate_on(&outside(&fx, "always"), None),
        )
        .await;
    std::fs::write(lease.path.join("undelivered.txt"), "x\n").unwrap();
    fx.complete(
        &worker,
        &undelivered.id,
        Some("test: complete worker attempt"),
    )
    .await;
    settle_by_hand(&fx, &undelivered.id).await;
    assert_eq!(
        current(&fx.boot, "undelivered")
            .await
            .status_detail
            .as_deref(),
        Some("delivery-failed")
    );
    refused(&fx, &undelivered.id, -32409, "(delivery-failed)").await;
    remove_pre_commit(&lease);
    undo_worker_changes(&lease.path);

    // A gate that checked no candidate (a legacy plain lease): unbound.
    let worker = fx.new_worker("unbound", AgentProvider::Codex).await;
    let dir = outside(&fx, "plain-lease");
    std::fs::create_dir_all(&dir).unwrap();
    calm_server::test_seams::acquire_workspace_lease_for_test(
        &fx.db(),
        &worker.card_id,
        fx.track(),
        "legacy-owner",
        &dir,
    )
    .await
    .unwrap();
    let unbound = fx
        .running_task(
            "unbound",
            "codex",
            &worker.card_id,
            gate_on(&outside(&fx, "never"), None),
        )
        .await;
    sqlx::query("UPDATE tasks SET status = 'verifying' WHERE id = ?1")
        .bind(&unbound.id)
        .execute(&fx.pool())
        .await
        .unwrap();
    drive(&fx, "unbound").await;
    assert_eq!(
        current(&fx.boot, "unbound").await.status,
        TaskStatus::Failed
    );
    refused(
        &fx,
        &unbound.id,
        -32409,
        "has no kernel-delivered candidate",
    )
    .await;
    fx.release_lease_by_hand(&worker.card_id).await;

    // A red gate whose context went stale.
    let (stale, _, _) = Box::pin(red(&fx, "stale", gate_on(&outside(&fx, "never"), None))).await;
    sqlx::query("UPDATE tasks SET context_stale_at_ms = 1 WHERE id = ?1")
        .bind(&stale.id)
        .execute(&fx.pool())
        .await
        .unwrap();
    refused(&fx, &stale.id, -32409, "context went stale").await;

    // A red gate a later task delivered past.
    let (passed_by, _, _) =
        Box::pin(red(&fx, "passed-by", gate_on(&outside(&fx, "never"), None))).await;
    let worker = fx.new_worker("later", AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&worker.card_id).await;
    let later = fx
        .running_task("later", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("later.txt"), "later\n").unwrap();
    fx.complete(&worker, &later.id, Some("test: complete worker attempt"))
        .await;
    settle_by_hand(&fx, &later.id).await;
    refused(
        &fx,
        &passed_by.id,
        -32409,
        "a later task delivered into the checkout",
    )
    .await;

    // A closed Track.
    call_tool(
        &fx.boot,
        "neige_track_close",
        planner_identity(&fx.boot),
        json!({"message": "done for now"}),
    )
    .await
    .unwrap();
    refused(&fx, &passed_by.id, -32409, "the track is closed").await;

    assert!(
        regate_events(&fx).await.is_empty(),
        "a refusal writes nothing"
    );
}

// ---------------------------------------------------------------------------
// T7: Planner-only at the tool (the role gate's own rule is pinned in calm-truth).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn regate_is_planner_only() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let (task, _, _) = red(&fx, "roles", gate_on(&outside(&fx, "green"), None)).await;
    let err = regate_as(&fx, fx.codex_worker(), &task.id)
        .await
        .expect_err("a worker cannot re-run a gate");
    assert_eq!(err.code, -32403, "{err:?}");
    assert_eq!(current(&fx.boot, "roles").await.status, TaskStatus::Failed);
    assert!(regate_events(&fx).await.is_empty());
}
