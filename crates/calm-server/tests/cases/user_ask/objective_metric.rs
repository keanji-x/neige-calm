//! Metrics use the command production path, including approval, revision checkout and settlement.
use super::*;

async fn metric_world(command: &str, role: &str) -> World {
    let w = world(command, |a| a).await;
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
