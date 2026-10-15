//! #2617: `truncated` reports the bounds that cut reported evidence and the producer's own
//! truncation warning; a run is read once however many of its shards failed; budget exhaustion
//! and non-FAILURE conclusions are named.
use super::*;

/// Every annotation the real renderer emits for `xml`, decoded as GitHub stores it.
fn rendered_annotations(xml: &str) -> Vec<Value> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = short_tempdir("x").unwrap();
    let path = dir.path().join("junit.xml");
    std::fs::write(&path, xml).unwrap();
    let output = std::process::Command::new("python3")
        .arg(root.join("scripts/ci/nextest-failure-annotations.py"))
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let decode = |s: &str| {
        s.replace("%3A", ":")
            .replace("%2C", ",")
            .replace("%0D", "\r")
            .replace("%0A", "\n")
            .replace("%25", "%")
    };
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| {
            let (level, rest) = line
                .strip_prefix("::")
                .unwrap()
                .split_once(" title=")
                .unwrap();
            let (title, message) = rest.split_once("::").unwrap();
            let level = if level == "error" { "failure" } else { level };
            json!({"annotation_level":level,"title":decode(title),"message":decode(message)})
        })
        .collect()
}

fn one_failed_check(cx: &ChecksFixture) {
    seed_actions(cx);
    seed_shim_pr_checks(
        &cx.fx.origin_repo,
        cx.pr,
        &json!([action_check(1, "COMPLETED")]),
    );
}

fn available(cx: &ChecksFixture) -> Value {
    let result = probe(cx);
    let diagnostics = result["failed_checks"][0]["diagnostics"].clone();
    assert_eq!(diagnostics["status"], "available", "{result}");
    diagnostics
}

fn failure_annotations(count: usize) -> Value {
    (0..count)
        .map(|i| json!({"annotation_level":"failure","title":"","message":format!("error {i}")}))
        .collect()
}

fn with_failed_steps(cx: &ChecksFixture, count: usize) {
    let mut jobs = jobs(cx);
    jobs["jobs"][0]["steps"] = (0..count)
        .map(|i| json!({"name":format!("step {i}"),"conclusion":"failure"}))
        .collect();
    api_fixture(cx, "actions/runs/100/attempts/2/jobs?per_page=100", jobs);
}

fn with_summary(cx: &ChecksFixture, summary: String) {
    api_fixture(
        cx,
        "check-runs/1",
        json!({
            "id":1,"node_id":"CheckRun-0","head_sha":cx.head_sha(),"status":"completed",
            "check_suite":{"id":90},"app":{"slug":"github-actions"},"output":{"summary":summary}
        }),
    );
}

#[tokio::test]
async fn gh_pr_checks_truncated_false_when_nothing_cut() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("nothing-cut").await;
    one_failed_check(&cx);
    api_fixture(
        &cx,
        "check-runs/1/annotations?per_page=100",
        json!([junit_annotation(1)]),
    );
    let diagnostics = available(&cx);
    assert_eq!(diagnostics["failed_tests"], json!([junit_test_name(1)]));
    assert_eq!(diagnostics["truncated"], false, "{diagnostics}");
}

#[tokio::test]
async fn gh_pr_checks_truncated_when_annotations_steps_or_summary_exceed_bounds() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("bounds-cut").await;
    let annotations = "check-runs/1/annotations?per_page=100";
    // Each bound first at its limit (nothing cut), then one past it.
    for (over, truncated) in [(0, false), (1, true)] {
        one_failed_check(&cx);
        api_fixture(&cx, annotations, failure_annotations(16 + over));
        assert_eq!(available(&cx)["truncated"], truncated, "annotations");

        one_failed_check(&cx);
        api_fixture(&cx, annotations, json!([]));
        with_failed_steps(&cx, 8 + over);
        let steps = available(&cx);
        assert_eq!(steps["failed_steps"].as_array().unwrap().len(), 8);
        assert_eq!(steps["truncated"], truncated, "steps: {steps}");

        one_failed_check(&cx);
        with_summary(&cx, "x".repeat(1000 + over));
        let summary = available(&cx);
        assert_eq!(summary["error_summary"], "x".repeat(1000));
        assert_eq!(summary["truncated"], truncated, "summary");

        // An empty check summary falls back to the annotations' joined messages.
        one_failed_check(&cx);
        with_summary(&cx, String::new());
        api_fixture(
            &cx,
            annotations,
            json!([{"annotation_level":"failure","title":"","message":"y".repeat(499 + over)},
                   {"annotation_level":"failure","title":"","message":"z".repeat(500)}]),
        );
        let joined = available(&cx);
        assert_eq!(joined["truncated"], truncated, "joined: {joined}");
    }
}

#[tokio::test]
async fn gh_pr_checks_truncated_when_producer_reports_truncation() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("producer-cut").await;
    let annotations = "check-runs/1/annotations?per_page=100";
    // A collected identity plus one the renderer refuses: it reports the cut itself.
    let incomplete = rendered_annotations(concat!(
        r#"<testsuite><testcase classname="crate-name::bin" name="module::case"><failure/></testcase>"#,
        r#"<testcase classname="bin" name="has space"><failure/></testcase></testsuite>"#
    ));
    assert_eq!(incomplete.len(), 2, "{incomplete:?}");
    one_failed_check(&cx);
    api_fixture(&cx, annotations, json!(incomplete));
    let diagnostics_cut = available(&cx);
    assert_eq!(
        diagnostics_cut["failed_tests"],
        json!(["crate-name::bin::module::case"])
    );
    assert_eq!(diagnostics_cut["truncated"], true, "{diagnostics_cut}");

    // The renderer's other warning reports nothing cut.
    let none_collected = rendered_annotations("<testsuite><testcase name=\"green\"/></testsuite>");
    assert_eq!(none_collected.len(), 1, "{none_collected:?}");
    assert_eq!(none_collected[0]["annotation_level"], "warning");
    one_failed_check(&cx);
    api_fixture(&cx, annotations, json!(none_collected));
    assert_eq!(available(&cx)["truncated"], false);
}

/// Run the read with a `date` that reports `later` seconds after the enrichment's first call.
fn probe_at(cx: &ChecksFixture, later: u64) -> Value {
    let dir = short_tempdir("d").unwrap();
    let count = dir.path().join("count");
    std::fs::write(
        dir.path().join("date"),
        format!(
            "#!/bin/sh\nif [ -f '{0}' ]; then echo {1}; else : > '{0}'; echo 1000; fi\n",
            count.display(),
            1000 + later
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        dir.path().join("date"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let payload = lower("gh_pr_checks", &json!({"repo":cx.repo_arg,"pr":cx.pr})).unwrap();
    let read: Vec<String> =
        serde_json::from_value(payload["probe"]["output_probe_argv"].clone()).unwrap();
    let gh_dir = cx._env._path_dir.path();
    let output = std::process::Command::new(&read[0])
        .args(&read[1..])
        .env(
            "PATH",
            format!(
                "{}:{}:{}",
                dir.path().display(),
                gh_dir.display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test]
async fn gh_pr_checks_budget_exhaustion_is_named() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("budget").await;
    seed_actions(&cx);
    seed_shim_pr_checks(
        &cx.fx.origin_repo,
        cx.pr,
        &json!([action_check(1, "COMPLETED"), action_check(2, "COMPLETED")]),
    );
    let exhausted = json!({"status":"unavailable","reason":"diagnostic time budget exhausted"});
    // Spent before a call starts.
    let result = probe_at(&cx, 40);
    for check in result["failed_checks"].as_array().unwrap() {
        assert_eq!(check["diagnostics"], exhausted, "{result}");
    }
    // Spent while a call runs: the client is timed out with the budget's last second.
    let state = shim_state_dir(&cx.fx.origin_repo);
    std::fs::write(state.join("block_api"), "").unwrap();
    let result = probe_at(&cx, 39);
    std::fs::write(state.join("release_api"), "").unwrap();
    for check in result["failed_checks"].as_array().unwrap() {
        assert_eq!(check["diagnostics"], exhausted, "{result}");
    }
}

#[tokio::test]
async fn gh_pr_checks_reads_one_run_once_for_many_shards() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("one-run").await;
    seed_actions(&cx);
    let mut jobs = jobs(&cx);
    let mut third = jobs["jobs"][1].clone();
    third["id"] = json!(503);
    third["check_run_url"] = json!(format!(
        "https://api.github.com/repos/{}/check-runs/3",
        cx.repo_arg
    ));
    jobs["jobs"].as_array_mut().unwrap().push(third);
    api_fixture(&cx, "actions/runs/100/attempts/2/jobs?per_page=100", jobs);
    api_fixture(
        &cx,
        "check-runs/3",
        json!({
            "id":3,"node_id":"CheckRun-2","head_sha":cx.head_sha(),"status":"completed",
            "check_suite":{"id":90},"app":{"slug":"github-actions"},"output":{"summary":"shard 3"}
        }),
    );
    seed_shim_pr_checks(
        &cx.fx.origin_repo,
        cx.pr,
        &Value::Array(
            [1, 2, 3]
                .map(|shard| action_check(shard, "COMPLETED"))
                .to_vec(),
        ),
    );
    let result = probe(&cx);
    for check in result["failed_checks"].as_array().unwrap() {
        assert_eq!(check["diagnostics"]["status"], "available", "{result}");
    }
    let log = std::fs::read_to_string(shim_state_dir(&cx.fx.origin_repo).join("gh.log")).unwrap();
    let calls = |suffix: &str| {
        let line = format!("api repos/{}/{suffix}", cx.repo_arg);
        log.lines().filter(|l| *l == line).count()
    };
    // One identity read and one closing attempt re-check of the shared run.
    assert_eq!(calls("actions/runs/100"), 2, "{log}");
    assert_eq!(calls("actions/runs/100/attempts/2/jobs?per_page=100"), 1);
    for shard in [1, 2, 3] {
        assert_eq!(calls(&format!("check-runs/{shard}")), 1);
        assert_eq!(
            calls(&format!("check-runs/{shard}/annotations?per_page=100")),
            1
        );
    }
}

#[tokio::test]
async fn gh_pr_checks_cancelled_check_names_its_conclusion() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("cancelled").await;
    seed_actions(&cx);
    let mut jobs = jobs(&cx);
    jobs["jobs"][0]["steps"] = json!([{"name":"test shard 1","conclusion":"cancelled"}]);
    api_fixture(&cx, "actions/runs/100/attempts/2/jobs?per_page=100", jobs);
    for (conclusion, reason) in [
        (
            "CANCELLED",
            "check concluded CANCELLED without a failed step",
        ),
        (
            "TIMED_OUT",
            "check concluded TIMED_OUT without a failed step",
        ),
        ("FAILURE", "no supported failure steps and error summary"),
    ] {
        let mut check = action_check(1, "COMPLETED");
        check["conclusion"] = json!(conclusion);
        seed_shim_pr_checks(&cx.fx.origin_repo, cx.pr, &json!([check]));
        let result = probe(&cx);
        assert_eq!(
            result["failed_checks"][0]["diagnostics"],
            json!({"status":"unavailable","reason":reason}),
            "{result}"
        );
    }
}
