//! #2617 item E (S3): a `gh_pr_checks` wait fenced with `after_rerun` keeps the rerun run's
//! checks pending until GitHub reports a later attempt that completed, so the failed attempt's
//! stale conclusion cannot end the wait or the deadline snapshot as a failure.
use super::*;

/// The rerun workflow run, and another run of the same head.
const RUN: u64 = 100;
const OTHER_RUN: u64 = 200;

/// One Actions CheckRun of workflow run `run`, as the GraphQL rollup reports it.
fn run_check(name: &str, run: u64, status: &str, conclusion: &str) -> Value {
    json!({
        "__typename": "CheckRun", "name": name, "status": status, "conclusion": conclusion,
        "detailsUrl": format!("https://github.invalid/shim/checks/{name}"), "databaseId": run + 1,
        "checkSuite": { "databaseId": run + 2, "workflowRun": { "databaseId": run } }
    })
}

/// What `GET actions/runs/{RUN}` reports.
fn seed_run(cx: &ChecksFixture, head_sha: &str, run_attempt: u64, status: &str) {
    let dir = shim_state_dir(&cx.fx.origin_repo).join("actions");
    std::fs::create_dir_all(&dir).unwrap();
    let endpoint = format!("repos/{}/actions/runs/{RUN}", cx.repo_arg);
    let run = json!({
        "id": RUN, "head_sha": head_sha, "run_attempt": run_attempt, "status": status,
        "repository": { "full_name": cx.repo_arg }
    });
    std::fs::write(dir.join(endpoint.replace('/', "_")), run.to_string()).unwrap();
}

/// The fenced call: run [`RUN`]'s attempt 1 failed and was rerun.
fn fenced_args(cx: &ChecksFixture, attempt: &str) -> Value {
    json!({
        "repo": cx.repo_arg, "pr": cx.pr, "attempt": attempt,
        "after_rerun": { "run_id": RUN, "run_attempt": 1 }
    })
}

async fn call_fenced(cx: &ChecksFixture, id: i64, attempt: &str) -> Value {
    call_tool(&cx.fx, id, PR_CHECKS_TOOL, fenced_args(cx, attempt)).await
}

/// The deadline snapshot: the lowered output probe of `args`, run once.
fn snapshot(cx: &ChecksFixture, args: &Value) -> std::process::Output {
    let payload = lower("gh_pr_checks", args).expect("checks payload");
    let read: Vec<String> =
        serde_json::from_value(payload["probe"]["output_probe_argv"].clone()).unwrap();
    run_checks_read(&cx._env._path_dir.path().join("gh"), &read)
}

fn failed_names(event: &Value) -> Vec<&str> {
    event["failed_checks"]
        .as_array()
        .expect("failed checks")
        .iter()
        .map(|check| check["name"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn a_fenced_wait_ignores_the_previous_attempts_failure() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("rerun-stale").await;
    // GitHub has not registered the rerun yet: the run still reports the failed attempt.
    seed_run(&cx, &cx.head_sha(), 1, "completed");
    seed_shim_pr_checks(
        &cx.fx.origin_repo,
        cx.pr,
        &json!([
            success("lint"),
            run_check("test", RUN, "COMPLETED", "FAILURE")
        ]),
    );
    let resp = call_fenced(&cx, 81, "r-1").await;
    let op_id = assert_receipt(&resp);
    let _child = parked_process_group_guard(&cx.fx.repo, &op_id).await;
    cx.assert_still_waiting(&op_id).await;

    // The rerun attempt failed too: that failure is reported.
    seed_run(&cx, &cx.head_sha(), 2, "completed");
    let event = cx.wait_for_event(Duration::from_secs(35)).await;
    assert_eq!(event.payload["conclusion"], "failure", "{event:?}");
    assert_eq!(failed_names(&event.payload), ["test"], "{event:?}");
    assert_eq!(event.payload["snapshot"]["head_sha"], cx.head_sha());
}

#[tokio::test]
async fn a_fenced_wait_settles_on_the_new_attempts_result() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("rerun-green").await;
    seed_run(&cx, &cx.head_sha(), 1, "completed");
    seed_shim_pr_checks(
        &cx.fx.origin_repo,
        cx.pr,
        &json!([
            success("lint"),
            run_check("test", RUN, "COMPLETED", "FAILURE")
        ]),
    );
    let resp = call_fenced(&cx, 82, "r-2").await;
    let op_id = assert_receipt(&resp);
    let _child = parked_process_group_guard(&cx.fx.repo, &op_id).await;
    cx.assert_still_waiting(&op_id).await;

    seed_run(&cx, &cx.head_sha(), 2, "completed");
    seed_shim_pr_checks(
        &cx.fx.origin_repo,
        cx.pr,
        &json!([
            success("lint"),
            run_check("test", RUN, "COMPLETED", "SUCCESS")
        ]),
    );
    let event = cx.wait_for_event(Duration::from_secs(35)).await;
    assert_eq!(event.payload["conclusion"], "success", "{event:?}");
    assert_eq!(event.payload["snapshot"]["all_checks_completed"], true);
    assert_eq!(event.payload["failed_checks"], json!([]));
}

#[tokio::test]
async fn a_fenced_wait_still_fails_on_other_runs() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("rerun-other").await;
    seed_run(&cx, &cx.head_sha(), 1, "completed");
    seed_shim_pr_checks(
        &cx.fx.origin_repo,
        cx.pr,
        &json!([
            run_check("test", RUN, "COMPLETED", "FAILURE"),
            run_check("other", OTHER_RUN, "COMPLETED", "FAILURE"),
            status_context("ci/legacy", "SUCCESS")
        ]),
    );
    let resp = call_fenced(&cx, 83, "r-3").await;
    let op_id = assert_receipt_or_settled(&resp);
    let _child = parked_process_group_guard(&cx.fx.repo, &op_id).await;
    let event = cx.wait_for_event(Duration::from_secs(10)).await;
    assert_eq!(event.payload["conclusion"], "failure", "{event:?}");
    // Only the other run's failure: the rerun run's stale check is pending.
    assert_eq!(failed_names(&event.payload), ["other"], "{event:?}");
    assert_eq!(event.payload["snapshot"]["all_checks_completed"], false);
}

#[tokio::test]
async fn a_fenced_deadline_snapshot_reports_pending() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let _deadline = EnvGuard::set("NEIGE_FORGE_DEADLINE_SECS", "1");
    let cx = ChecksFixture::boot("rerun-deadline").await;
    seed_run(&cx, &cx.head_sha(), 1, "completed");
    seed_shim_pr_checks(
        &cx.fx.origin_repo,
        cx.pr,
        &json!([
            success("lint"),
            run_check("test", RUN, "COMPLETED", "FAILURE")
        ]),
    );
    let resp = call_fenced(&cx, 84, "r-4").await;
    let op_id = assert_receipt(&resp);
    let _child = parked_process_group_guard(&cx.fx.repo, &op_id).await;
    cx.assert_still_waiting(&op_id).await;

    cx.fx._runtime.sweep_parked().await.expect("parked sweep");
    assert_eq!(operation_phase(&cx.fx.repo, &op_id).await, "succeeded");
    let event = cx.wait_for_event(Duration::from_secs(5)).await;
    assert_eq!(event.payload["conclusion"], "pending", "{event:?}");
    assert_eq!(event.payload["snapshot"]["all_checks_completed"], false);
    assert_eq!(event.payload["failed_checks"], json!([]));
}

/// The fence can only turn the rerun run's checks pending: never green while the rerun is
/// incomplete, and without a fence the read is unchanged.
#[tokio::test]
async fn a_fenced_read_never_yields_success_while_the_rerun_is_incomplete() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("rerun-table").await;
    let head = cx.head_sha();
    let fenced = fenced_args(&cx, "table");
    let unfenced = json!({ "repo": cx.repo_arg, "pr": cx.pr, "attempt": "table" });
    let cases = [
        // (case, fenced?, observed attempt, run status, the run's check conclusion, verdict)
        ("stale-failure", true, 1, "completed", "FAILURE", "pending"),
        ("rerun-queued", true, 2, "queued", "FAILURE", "pending"),
        (
            "green-before-completion",
            true,
            2,
            "in_progress",
            "SUCCESS",
            "pending",
        ),
        (
            "green-same-attempt",
            true,
            1,
            "completed",
            "SUCCESS",
            "pending",
        ),
        (
            "new-attempt-failed",
            true,
            2,
            "completed",
            "FAILURE",
            "failure",
        ),
        (
            "new-attempt-green",
            true,
            3,
            "completed",
            "SUCCESS",
            "success",
        ),
        (
            "unfenced-stale-failure",
            false,
            1,
            "completed",
            "FAILURE",
            "failure",
        ),
        (
            "unfenced-green",
            false,
            1,
            "in_progress",
            "SUCCESS",
            "success",
        ),
    ];
    let mut mismatches = Vec::new();
    for (case, is_fenced, attempt, status, conclusion, want) in cases {
        seed_run(&cx, &head, attempt, status);
        seed_shim_pr_checks(
            &cx.fx.origin_repo,
            cx.pr,
            &json!([
                success("lint"),
                run_check("test", RUN, "COMPLETED", conclusion)
            ]),
        );
        let output = snapshot(&cx, if is_fenced { &fenced } else { &unfenced });
        match serde_json::from_slice::<Value>(&output.stdout) {
            Ok(got) if output.status.success() && got["conclusion"] == want => {}
            got => mismatches.push(format!("{case}: expected {want}, got {got:?} ({output:?})")),
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

#[tokio::test]
async fn a_fenced_wait_ends_on_head_move() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("rerun-moves").await;
    let first_head = cx.head_sha();
    seed_run(&cx, &first_head, 1, "completed");
    seed_shim_pr_checks(
        &cx.fx.origin_repo,
        cx.pr,
        &json!([run_check("test", RUN, "COMPLETED", "FAILURE")]),
    );
    let resp = call_fenced(&cx, 85, "r-5").await;
    let op_id = assert_receipt(&resp);
    let _child = parked_process_group_guard(&cx.fx.repo, &op_id).await;
    cx.assert_still_waiting(&op_id).await;

    let moved = cx.commit_to_pr_branch();
    assert_ne!(moved, first_head);
    let event = cx.wait_for_event(Duration::from_secs(35)).await;
    assert_eq!(event.payload["snapshot"]["head_sha"], moved, "{event:?}");
    assert_eq!(event.payload["conclusion"], "pending", "{event:?}");
}

/// A fenced run that is not of the PR head is refused: the wait ends without a wake, and the
/// deadline snapshot emits no evidence.
#[tokio::test]
async fn a_fenced_wait_refuses_a_run_of_another_head() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("rerun-wrong-head").await;
    seed_run(&cx, &"0".repeat(40), 2, "completed");
    seed_shim_pr_checks(
        &cx.fx.origin_repo,
        cx.pr,
        &json!([run_check("test", RUN, "COMPLETED", "SUCCESS")]),
    );
    let output = snapshot(&cx, &fenced_args(&cx, "r-6"));
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");

    let resp = call_fenced(&cx, 86, "r-6").await;
    let op_id = resp["result"]["structuredContent"]["op_id"]
        .as_str()
        .unwrap_or_else(|| panic!("an op: {resp}"))
        .to_owned();
    let deadline = Instant::now() + Duration::from_secs(20);
    while operation_phase(&cx.fx.repo, &op_id).await == "parked" {
        assert!(Instant::now() < deadline, "the refused wait never ended");
        sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(operation_phase(&cx.fx.repo, &op_id).await, "failed");
    assert!(cx.events().await.is_empty(), "{:?}", cx.events().await);
}
