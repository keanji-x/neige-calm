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

/// A recovered gate whose cleanup failed: a descendant carrying `#g1`'s marker outlived its dead
/// wrapper and hid its environ (`PR_SET_DUMPABLE=0`), so nothing can authenticate or kill it, and
/// `#g1` is still a terminal op. The descendant leads its own group; `#g1`'s recorded pgid is
/// pointed at it, the shape a recovery leaves when the wrapper died first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn regate_refused_while_a_previous_gate_process_survives() {
    use std::os::unix::process::CommandExt as _;
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let (task, _, _) = red(&fx, "survivor", gate_on(&outside(&fx, "green"), None)).await;
    let mut hidden = std::process::Command::new("python3")
        .arg("-c")
        .arg("import ctypes,time; ctypes.CDLL(None).prctl(4,0,0,0,0); time.sleep(30)")
        .env("NEIGE_GATE_OP", format!("{}#g1", task.id))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn the hidden-environ descendant");
    let hidden_pid = hidden.id();
    let hidden_env = tokio::time::timeout(WAIT, async {
        loop {
            let read = std::fs::read(format!("/proc/{hidden_pid}/environ"));
            if read.err().and_then(|error| error.raw_os_error()) == Some(libc::EACCES) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    sqlx::query(
        "UPDATE operations SET spawn_artifacts_json = json_set(spawn_artifacts_json, '$.pgid', ?1) \
         WHERE kind = ?2 AND idempotency_key = ?3",
    )
    .bind(i64::from(hidden_pid))
    .bind(TASK_VERIFY_KIND)
    .bind(format!("{}#g1", task.id))
    .execute(&fx.pool())
    .await
    .unwrap();

    let result = regate(&fx, &task.id).await;
    let _ = hidden.kill();
    let _ = hidden.wait();
    assert!(
        hidden_env.is_ok(),
        "the descendant's environ never became unreadable"
    );
    let err = result.expect_err("a surviving gate process refuses the re-run");
    assert_eq!(err.code, -32409, "{err:?}");
    assert!(
        err.message
            .contains("the previous gate's processes (attempt 1) could not be proven stopped"),
        "{err:?}"
    );
    assert_eq!(
        current(&fx.boot, "survivor").await.status,
        TaskStatus::Failed
    );
    assert!(regate_events(&fx).await.is_empty());
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
    fx.complete(&worker, &undelivered.id).await;
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
        &fx.pool(),
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
    let (stale, _, _) = red(&fx, "stale", gate_on(&outside(&fx, "never"), None)).await;
    sqlx::query("UPDATE tasks SET context_stale_at_ms = 1 WHERE id = ?1")
        .bind(&stale.id)
        .execute(&fx.pool())
        .await
        .unwrap();
    refused(&fx, &stale.id, -32409, "context went stale").await;

    // A red gate a later task delivered past.
    let (passed_by, _, _) = red(&fx, "passed-by", gate_on(&outside(&fx, "never"), None)).await;
    let worker = fx.new_worker("later", AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&worker.card_id).await;
    let later = fx
        .running_task("later", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("later.txt"), "later\n").unwrap();
    fx.complete(&worker, &later.id).await;
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
