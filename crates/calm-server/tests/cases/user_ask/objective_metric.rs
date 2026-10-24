//! Metrics use the command production path, including approval, revision checkout and settlement.
use super::*;

async fn metric_world(command: &str, role: &str) -> World {
    metric_world_with(command, role, |a| a).await
}
async fn metric_world_with(
    command: &str,
    role: &str,
    hook: impl FnOnce(ObjectiveCheckRunAdapter) -> ObjectiveCheckRunAdapter,
) -> World {
    let w = world(command, hook).await;
    let mut d = definition();
    d["mode"] = if role == "goal" {
        json!("converge")
    } else {
        json!({"loop":{"plateau":10}})
    };
    d["checks"][0]["role"] = json!(role);
    d["checks"][0]["config"] =
        json!({"kind":"metric","params":{"command":command,"direction":"lower"}});
    define(&w.boot, Some(w.block.clone()), d).await;
    w
}
fn metric_value(s: &objective::State) -> f64 {
    let objective::engine::Evaluation::Ready { results, .. } = s.objectives
        [&objective::ids::ObjectiveKey::new("kr")]
        .evaluation
        .as_ref()
        .unwrap()
    else {
        panic!("metric not ready")
    };
    let objective::check::CheckOutput::Metric(output) =
        results[&objective::ids::CheckId::new("accepted")]
    else {
        panic!("not a metric")
    };
    output.value
}
async fn current(w: &World) -> Value {
    project(w).await;
    w.boot
        .repo
        .overlays_for("track", w.boot.track_id.as_str())
        .await
        .unwrap()
        .into_iter()
        .find(|o| o.kind == "objective.kr")
        .unwrap()
        .payload["cell"]["table"]["rows"][0]["current"]
        .clone()
}

#[tokio::test]
async fn objective_metric_stdout_separate_at_revision() {
    let w = metric_world(
        "test \"$(cat marker)\" = 1 && printf 'warming\\n3.5\\n'; printf 'stderr tail\\n' >&2",
        "ratchet",
    )
    .await;
    commit(&w.repo, "2");
    approve(&w).await;
    evaluate(&w, &w.revision).await.unwrap();
    ready(&w).await;
    assert_eq!(metric_value(&state(&w).await), 3.5);
    assert_eq!(count(&w, "check_finished").await, 1);
    assert_eq!(current(&w).await, "accepted: 3.5（基线：—）");
    let logs = std::fs::read_dir(w._tmp.path().join("logs")).unwrap();
    let log = logs
        .filter_map(Result::ok)
        .find(|e| e.path().extension().is_some_and(|x| x == "log"))
        .unwrap();
    let log = std::fs::read_to_string(log.path()).unwrap();
    assert!(log.contains("stderr tail"));
    assert!(!log.contains("warming"));
}

#[tokio::test]
async fn objective_metric_lower_regression_and_baseline() {
    let w = metric_world(
        "if [ \"$(cat marker)\" = 1 ]; then echo 5.0; else echo 6.0; fi",
        "ratchet",
    )
    .await;
    approve(&w).await;
    evaluate(&w, &w.revision).await.unwrap();
    ready(&w).await;
    assert_eq!(metric_value(&state(&w).await), 5.0);
    call_tool(
        &w.boot,
        "neige_objective_settle",
        json!({"key":"kr","evaluation":0,"decision":"keep","note":"baseline"}),
    )
    .await
    .unwrap();
    let next = commit(&w.repo, "2");
    evaluate(&w, &next).await.unwrap();
    ready(&w).await;
    assert_eq!(metric_value(&state(&w).await), 6.0);
    assert_eq!(current(&w).await, "accepted: 6（基线：5）");
    call_tool(
        &w.boot,
        "neige_objective_settle",
        json!({"key":"kr","evaluation":1,"decision":"keep","note":"refuse regression"}),
    )
    .await
    .unwrap();
    assert_eq!(
        state(&w).await.objectives[&objective::ids::ObjectiveKey::new("kr")]
            .ledger
            .entries
            .len(),
        1
    );
    project(&w).await;
    assert!(
        rows(&w.boot, "track.wake_requested")
            .await
            .iter()
            .any(|(_, r)| r.payload_value()["text"]
                .as_str()
                .unwrap()
                .contains("SettleRefused"))
    );
}

#[tokio::test]
async fn objective_metric_invalid_output_failed_wakes_once() {
    for command in [
        "echo not-a-number",
        "echo 3.5; exit 7",
        "head -c 70000 /dev/zero; echo 3.5",
    ] {
        let w = metric_world(command, "ratchet").await;
        approve(&w).await;
        evaluate(&w, &w.revision).await.unwrap();
        settled(&w).await;
        project(&w).await;
        project(&w).await;
        assert_eq!(count(&w, "check_finished").await, 0, "{command}");
        assert_eq!(phases(&w).await.len(), 3);
        for file in std::fs::read_dir(w._tmp.path().join("logs"))
            .unwrap()
            .filter_map(Result::ok)
        {
            if file.path().extension().is_some_and(|ext| ext == "stdout") {
                assert!(file.metadata().unwrap().len() <= 64 * 1024 + 1);
            }
        }
        let statuses: Vec<String> = sqlx::query_scalar(
            "SELECT settlement_json FROM objective_intents WHERE settlement_json IS NOT NULL",
        )
        .fetch_all(w.boot.repo.sqlite_db().unwrap().read_pool())
        .await
        .unwrap();
        assert!(
            statuses.iter().any(|s| s.contains("failed")),
            "{statuses:?}"
        );
        assert_eq!(
            rows(&w.boot, "track.wake_requested")
                .await
                .iter()
                .filter(|(_, r)| r.payload_value()["source"] == KIND)
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn objective_metric_goal_rejected_without_events() {
    let w = metric_world("echo 3.5", "goal").await;
    approve(&w).await;
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM objective_events")
        .fetch_one(w.boot.repo.sqlite_db().unwrap().read_pool())
        .await
        .unwrap();
    let error = evaluate(&w, &w.revision).await.unwrap_err();
    assert!(
        error
            .message
            .contains("numeric target as metric Goal is not yet supported"),
        "{error:?}"
    );
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM objective_events")
        .fetch_one(w.boot.repo.sqlite_db().unwrap().read_pool())
        .await
        .unwrap();
    assert_eq!(before, after);
    assert!(phases(&w).await.is_empty());
}

async fn assert_metric_failed_clean(w: &World) {
    settled(w).await;
    project(w).await;
    project(w).await; // Idempotent cleanup and settlement on the next tick.
    assert_eq!(phases(w).await.len(), 3);
    assert_eq!(count(w, "check_finished").await, 0);
    assert_eq!(
        rows(&w.boot, "track.wake_requested")
            .await
            .iter()
            .filter(|(_, r)| r.payload_value()["source"] == KIND)
            .count(),
        1
    );
    let outputs: Vec<String> =
        sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE kind=?")
            .bind(KIND)
            .fetch_all(w.boot.repo.sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
    for raw in outputs {
        let output: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(output["result"]["status"], "failed");
        assert_eq!(output["data"]["checkout_removed"], true);
        let stdout = Path::new(output["data"]["stdout"].as_str().unwrap());
        assert!(
            !stdout.with_extension("stdout.pipe").exists(),
            "FIFO remains: {stdout:?}"
        );
        assert!(!stdout.exists(), "temporary stdout remains: {stdout:?}");
        assert!(!Path::new(output["data"]["cwd"].as_str().unwrap()).exists());
    }
}

#[tokio::test]
async fn objective_metric_timeout_cleans_capture_and_wakes_once() {
    let w = metric_world_with("echo 3.5; sleep 60", "ratchet", |a| a.with_timeout(1)).await;
    approve(&w).await;
    evaluate(&w, &w.revision).await.unwrap();
    assert_metric_failed_clean(&w).await;
}

#[tokio::test]
async fn objective_metric_inherited_stdout_cleans_capture_and_wakes_once() {
    let w = metric_world_with("sleep 60 & echo 3.5", "ratchet", |a| a.with_timeout(1)).await;
    approve(&w).await;
    evaluate(&w, &w.revision).await.unwrap();
    assert_metric_failed_clean(&w).await;
}

#[test]
fn objective_metric_running_restart_cleans_capture_and_wakes_once() {
    let marker = tempfile::NamedTempFile::new().unwrap();
    let marker_path = marker.path().to_owned();
    let initial = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut w = initial.block_on(async {
        let command = format!(
            "echo 3.5; echo released > '{}'; sleep 60",
            marker_path.display()
        );
        let w = metric_world_with(&command, "ratchet", |a| a.with_timeout(10)).await;
        approve(&w).await;
        evaluate(&w, &w.revision).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if std::fs::read_to_string(&marker_path).unwrap() == "released\n" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("metric released and writing stdout before kernel crash");
        assert_eq!(phases(&w).await, ["parked"]);
        w
    });
    // Cancel the kernel's tasks while the independent supervisor and command live.
    drop(initial);
    let restarted = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    restarted.block_on(async {
        let engine = w
            ._execution
            .new_kernel(w.boot.repo.sqlite_db().unwrap(), false)
            .await;
        let lifecycle = calm_server::execution_quiesce::ExecutionLifecycle::default();
        lifecycle.bind(Arc::new(engine));
        w.runtime = operation_runtime(
            &w.boot,
            ObjectiveCheckRunAdapter::new(w.checks.clone(), w._tmp.path().join("logs"))
                .with_timeout(1),
            lifecycle,
        );
        sqlx::query("UPDATE operations SET lease_until_ms=0 WHERE kind=?")
            .bind(KIND)
            .execute(w.boot.repo.sqlite_db().unwrap().writer_pool())
            .await
            .unwrap();
        let plan = w.runtime.recover_on_boot().await.unwrap();
        w.runtime.apply_recovery(plan).await.unwrap();
        assert_metric_failed_clean(&w).await;
        let raw: String = sqlx::query_scalar(
            "SELECT tx_output_json FROM operations WHERE kind=? ORDER BY created_at_ms,id LIMIT 1",
        )
        .bind(KIND)
        .fetch_one(w.boot.repo.sqlite_db().unwrap().read_pool())
        .await
        .unwrap();
        assert!(raw.contains("KernelRestarted"), "{raw}");
    });
}

#[tokio::test]
async fn objective_metric_existing_goal_requested_reconciles_without_operations() {
    let w = metric_world("echo 3.5", "goal").await;
    approve(&w).await;
    // A historical Requested intent enters through the host, bypassing today's MCP admission.
    objective_host::react(
        &w.boot.repo.sqlite_db().unwrap(),
        objective_host::ObjectiveInput {
            track_id: w.boot.track_id.clone(),
            event: objective::Event::Requested {
                objective: objective::ids::ObjectiveKey::new("kr"),
                revision: objective::ids::Revision::new(&w.revision),
            },
            source: objective_host::InputSource::Planner {
                principal: calm_types::worker::Principal::Agent {
                    session_id: PLANNER_SESSION_ID.into(),
                    track_id: w.boot.track_id.clone(),
                    area_id: w.boot.area_id.clone(),
                },
                request_id: objective_host::PlannerRequestId::new("historical-metric-goal"),
            },
        },
    )
    .await
    .unwrap();
    let before = count(&w, "requested").await;
    assert_eq!(before, 1);
    let runtime = operation_runtime(
        &w.boot,
        ObjectiveCheckRunAdapter::new(w.checks.clone(), w._tmp.path().join("logs")),
        w._execution.lifecycle.clone(),
    );
    let plan = runtime.recover_on_boot().await.unwrap();
    runtime.apply_recovery(plan).await.unwrap();
    for _ in 0..5 {
        project(&w).await;
    }
    assert!(phases(&w).await.is_empty());
    assert_eq!(count(&w, "check_finished").await, 0);
    assert_eq!(count(&w, "requested").await, before);
    assert_eq!(
        rows(&w.boot, "track.wake_requested")
            .await
            .iter()
            .filter(|(_, r)| r.payload_value()["source"] == KIND)
            .count(),
        0
    );
    let statuses: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT status,settlement_json FROM objective_intents WHERE kind='run_check'",
    )
    .fetch_all(w.boot.repo.sqlite_db().unwrap().read_pool())
    .await
    .unwrap();
    assert_eq!(statuses, [("pending".into(), None)]);
}

#[test]
fn objective_metric_boot_cleans_previously_acknowledged_capture() {
    let initial = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (w, writer) = initial.block_on(async {
        let w = metric_world("echo 3.5", "ratchet").await;
        approve(&w).await;
        evaluate(&w, &w.revision).await.unwrap();
        ready(&w).await;
        settled(&w).await;
        let writer = w
            .boot
            .repo
            .sqlite_db()
            .unwrap()
            .writer_pool()
            .acquire()
            .await
            .unwrap();
        (w, writer)
    });
    // Quiesce old cleanup tasks before injecting a durable pre-fix orphan.
    drop(initial);
    let restarted = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    restarted.block_on(async {
        drop(writer);
        let db = w.boot.repo.sqlite_db().unwrap();
        let raw: String = sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE kind=?")
            .bind(KIND).fetch_one(db.read_pool()).await.unwrap();
        let output: Value = serde_json::from_str(&raw).unwrap();
        let stdout = Path::new(output["data"]["stdout"].as_str().unwrap());
        let pipe = stdout.with_extension("stdout.pipe");
        std::fs::write(stdout, "3.5").unwrap();
        assert!(std::process::Command::new("mkfifo").arg(&pipe).status().unwrap().success());
        sqlx::query("UPDATE operations SET \
             tx_output_json=json_remove(json_set(tx_output_json,'$.data.checkout_removed',json('true')),'$.data.capture_removed') WHERE kind=?")
            .bind(KIND).execute(db.writer_pool()).await.unwrap();
        project(&w).await; // Startup reconciliation of a terminal pre-fix row.
        assert!(!pipe.exists());
        assert!(!stdout.exists());
        project(&w).await; // Tick is idempotent after the new ACK.
        assert_eq!(count(&w, "check_finished").await, 1);
        assert_eq!(phases(&w).await, ["succeeded"]);
        assert_eq!(rows(&w.boot, "track.wake_requested").await.iter()
            .filter(|(_, r)| r.payload_value()["source"] == KIND).count(), 0);
    });
}
