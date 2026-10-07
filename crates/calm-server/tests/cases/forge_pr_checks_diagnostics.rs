use super::*;

fn api_fixture(cx: &ChecksFixture, suffix: &str, value: Value) {
    let dir = shim_state_dir(&cx.fx.origin_repo).join("actions");
    std::fs::create_dir_all(&dir).unwrap();
    let endpoint = format!("repos/{}/{suffix}", cx.repo_arg);
    std::fs::write(dir.join(endpoint.replace('/', "_")), value.to_string()).unwrap();
}

fn action_check(shard: u64, status: &str) -> Value {
    json!({
        "__typename":"CheckRun", "name":format!("shard-{shard}"),
        "status":status, "conclusion":if status == "COMPLETED" {"FAILURE"} else {""},
        "detailsUrl":"https://untrusted.example/details", "databaseId":shard,
        "checkSuite":{"databaseId":90,"workflowRun":{"databaseId":100}}
    })
}

fn seed_actions(cx: &ChecksFixture) {
    api_fixture(
        cx,
        "actions/runs/100",
        json!({
            "id":100,"head_sha":cx.head_sha(),"run_attempt":2,"check_suite_id":90,
            "repository":{"full_name":cx.repo_arg}
        }),
    );
    for shard in [1, 2] {
        api_fixture(
            cx,
            &format!("check-runs/{shard}"),
            json!({
                "id":shard,"node_id":format!("CheckRun-{}",shard-1), "head_sha":cx.head_sha(),"status":"completed",
                "check_suite":{"id":90},"app":{"slug":"github-actions"},
                "output":{"summary":format!("assertion failed in shard {shard}")}
            }),
        );
    }
    api_fixture(
        cx,
        "actions/runs/100/attempts/2/jobs?per_page=100",
        jobs(cx),
    );
}

fn junit_test_name(shard: usize) -> &'static str {
    match shard {
        1 => include_str!("../../../../scripts/ci/fixtures/nextest-failures/shard1-name.txt"),
        2 => include_str!("../../../../scripts/ci/fixtures/nextest-failures/shard2-name.txt"),
        _ => panic!("unknown captured shard"),
    }
    .trim_end()
}

// Decode the workflow command exactly as GitHub does, then exercise the real
// plugin reader with structured annotation.title/message fields (not a summary).
fn junit_annotation(shard: u64) -> Value {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("scripts/ci/nextest-failure-annotations.py"))
        .arg(root.join(format!(
            "scripts/ci/fixtures/nextest-failures/shard{shard}.xml"
        )))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let output = String::from_utf8(output.stdout).unwrap();
    let line = output
        .lines()
        .find(|line| line.starts_with("::error title="))
        .unwrap();
    let (title, message) = line
        .strip_prefix("::error title=")
        .unwrap()
        .split_once("::")
        .unwrap();
    let decode = |s: &str| {
        s.replace("%3A", ":")
            .replace("%2C", ",")
            .replace("%0D", "\r")
            .replace("%0A", "\n")
            .replace("%25", "%")
    };
    json!({"annotation_level":"failure","title":decode(title),"message":decode(message)})
}

fn jobs(cx: &ChecksFixture) -> Value {
    let entries = [1,2].map(|shard| json!({
        "id":500+shard,"name":"same job name","run_id":100,"run_attempt":2,"head_sha":cx.head_sha(),"status":"completed",
        "run_url":format!("https://api.github.com/repos/{}/actions/runs/100",cx.repo_arg),
        "check_run_url":format!("https://api.github.com/repos/{}/check-runs/{shard}",cx.repo_arg),
        "steps":[{"name":format!("test shard {shard}"),"conclusion":"failure"}]
    }));
    json!({"jobs": entries})
}

fn probe(cx: &ChecksFixture) -> Value {
    let payload = lower("gh_pr_checks", &json!({"repo":cx.repo_arg,"pr":cx.pr})).unwrap();
    let read: Vec<String> =
        serde_json::from_value(payload["probe"]["output_probe_argv"].clone()).unwrap();
    let output = run_checks_read(&cx._env._path_dir.path().join("gh"), &read);
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test]
async fn gh_pr_checks_all_empty_deadline_is_partial() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let _deadline = EnvGuard::set("NEIGE_FORGE_DEADLINE_SECS", "1");
    let cx = ChecksFixture::boot("empty-all").await;
    seed_shim_pr_checks(&cx.fx.origin_repo, cx.pr, &json!([]));
    let resp = call_tool(
        &cx.fx,
        63,
        PR_CHECKS_TOOL,
        json!({"repo":cx.repo_arg,"pr":cx.pr,"attempt":"empty-all","wait_for_all":true}),
    )
    .await;
    let op_id = assert_receipt(&resp);
    let _child = parked_process_group_guard(&cx.fx.repo, &op_id).await;
    cx.assert_still_waiting(&op_id).await;
    cx.fx._runtime.sweep_parked().await.unwrap();
    let event = cx.wait_for_event(Duration::from_secs(5)).await;
    assert_eq!(event.payload["conclusion"], "no_checks");
    assert_eq!(event.payload["snapshot"]["all_checks_completed"], false);
    assert_eq!(event.payload["failed_checks"], json!([]));
}

#[tokio::test]
async fn gh_pr_checks_all_token_aliases_redacted_through_persistence_replay_wake() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let secrets: Vec<_> = calm_types::forge_env::FORGE_CREDENTIAL_ENV_KEYS
        .iter()
        .filter(|key| key.ends_with("TOKEN"))
        .enumerate()
        .map(|(i, key)| {
            (
                *key,
                format!("0123456789abcdef0123456789abcdef0123456{i:03}"),
            )
        })
        .collect();
    let _guards: Vec<_> = secrets
        .iter()
        .map(|(key, secret)| EnvGuard::set(key, secret))
        .collect();
    let cx = ChecksFixture::boot("token-aliases").await;
    seed_actions(&cx);
    let text = secrets
        .iter()
        .map(|(_, s)| s.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    api_fixture(
        &cx,
        "check-runs/1",
        json!({"id":1,"node_id":"CheckRun-0","head_sha":cx.head_sha(),"status":"completed",
        "check_suite":{"id":90},"app":{"slug":"github-actions"},"output":{"summary":text}}),
    );
    let mut job = jobs(&cx);
    job["jobs"][0]["steps"][0]["name"] = json!(text);
    api_fixture(&cx, "actions/runs/100/attempts/2/jobs?per_page=100", job);
    api_fixture(&cx, "check-runs/1/annotations?per_page=100", json!([]));
    let mut check = action_check(1, "COMPLETED");
    check["name"] = json!(text);
    seed_shim_pr_checks(&cx.fx.origin_repo, cx.pr, &json!([check]));
    let summary_read = probe(&cx);
    assert_eq!(
        summary_read["failed_checks"][0]["diagnostics"]["status"],
        "available"
    );
    for (_, secret) in &secrets {
        assert!(
            !summary_read.to_string().contains(secret),
            "summary leaked {secret}"
        );
    }
    api_fixture(
        &cx,
        "check-runs/1/annotations?per_page=100",
        json!([{"annotation_level":"failure","title":format!("nextest failure: binary::{text}"),"message":text}]),
    );
    let read = probe(&cx);
    assert_eq!(
        read["failed_checks"][0]["diagnostics"]["status"], "available",
        "{read}"
    );
    assert!(
        !read["failed_checks"][0]["diagnostics"]["failed_tests"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{read}"
    );
    for (_, secret) in &secrets {
        assert!(!read.to_string().contains(secret), "read leaked {secret}");
    }
    let args = json!({"repo":cx.repo_arg,"pr":cx.pr,"attempt":"tokens","wait_for_all":true});
    let resp = call_tool(&cx.fx, 60, PR_CHECKS_TOOL, args.clone()).await;
    let op_id = assert_receipt_or_settled(&resp);
    let _child = parked_process_group_guard(&cx.fx.repo, &op_id).await;
    let event = cx.wait_for_event(Duration::from_secs(10)).await;
    let replay = call_tool(&cx.fx, 61, PR_CHECKS_TOOL, args).await;
    let replayed = calm_types::event::Event::from_kind_and_payload(
        "forge.pr.checks",
        replay["result"]["structuredContent"]["result"]["event"].clone(),
    )
    .unwrap();
    assert_eq!(replayed.payload_value(), event.payload);
    let wake = checks_wake(&event.payload);
    for (_, secret) in &secrets {
        assert!(!event.payload.to_string().contains(secret));
        assert!(!replayed.payload_value().to_string().contains(secret));
        assert!(!wake.contains(secret));
    }
}

fn checks_wake(payload: &Value) -> String {
    let mut observation = payload.clone();
    observation["type"] = json!("forge_pr_checks");
    serde_json::from_value::<calm_types::observation::Observation>(observation)
        .unwrap()
        .to_turn_text()
}

#[tokio::test]
async fn gh_pr_checks_all_conflict_and_headmove_preserve_partial_empty_snapshot() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    for conflicting in [true, false] {
        let cx = ChecksFixture::boot(if conflicting {
            "all-conflict"
        } else {
            "all-headmove"
        })
        .await;
        seed_shim_pr_checks(&cx.fx.origin_repo, cx.pr, &json!([]));
        if conflicting {
            seed_shim_pr_mergeable(&cx.fx.origin_repo, cx.pr, "CONFLICTING");
        }
        let resp = call_tool(
            &cx.fx,
            60,
            PR_CHECKS_TOOL,
            json!({"repo":cx.repo_arg,"pr":cx.pr,"attempt":"all-boundary","wait_for_all":true}),
        )
        .await;
        let op_id = assert_receipt_or_settled(&resp);
        let _child = parked_process_group_guard(&cx.fx.repo, &op_id).await;
        let head = if conflicting {
            cx.head_sha()
        } else {
            cx.assert_still_waiting(&op_id).await;
            cx.commit_to_pr_branch()
        };
        let event = cx.wait_for_event(Duration::from_secs(35)).await;
        assert_eq!(event.payload["conclusion"], "no_checks");
        assert_eq!(event.payload["snapshot"]["head_sha"], head);
        assert_eq!(event.payload["snapshot"]["all_checks_completed"], false);
        assert_eq!(
            event.payload["snapshot"]["mergeable"],
            if conflicting {
                "conflicting"
            } else {
                "mergeable"
            }
        );
    }
}

#[tokio::test]
async fn gh_pr_checks_all_deadline_records_partial_diagnostics() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let _deadline = EnvGuard::set("NEIGE_FORGE_DEADLINE_SECS", "1");
    let cx = ChecksFixture::boot("all-deadline").await;
    seed_actions(&cx);
    seed_shim_pr_checks(
        &cx.fx.origin_repo,
        cx.pr,
        &json!([action_check(1, "COMPLETED"), action_check(2, "IN_PROGRESS")]),
    );
    let resp = call_tool(
        &cx.fx,
        62,
        PR_CHECKS_TOOL,
        json!({"repo":cx.repo_arg,"pr":cx.pr,"attempt":"deadline-all","wait_for_all":true}),
    )
    .await;
    let op_id = assert_receipt(&resp);
    let _child = parked_process_group_guard(&cx.fx.repo, &op_id).await;
    cx.assert_still_waiting(&op_id).await;
    cx.fx._runtime.sweep_parked().await.unwrap();
    let event = cx.wait_for_event(Duration::from_secs(5)).await;
    assert_eq!(event.payload["conclusion"], "failure");
    assert_eq!(event.payload["snapshot"]["all_checks_completed"], false);
    assert_eq!(event.payload["failed_checks"].as_array().unwrap().len(), 1);
    assert_eq!(
        event.payload["failed_checks"][0]["diagnostics"]["status"], "available",
        "{event:?}"
    );
}

#[tokio::test]
async fn gh_pr_checks_all_waits_for_two_shards_and_replays_diagnostics() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("all-diagnostics").await;
    seed_actions(&cx);
    for shard in [1, 2] {
        api_fixture(
            &cx,
            &format!("check-runs/{shard}/annotations?per_page=100"),
            json!([junit_annotation(shard)]),
        );
    }
    seed_shim_pr_checks(
        &cx.fx.origin_repo,
        cx.pr,
        &json!([action_check(1, "COMPLETED"), action_check(2, "IN_PROGRESS")]),
    );
    seed_shim_pr_checks(&cx.fx.origin_repo, cx.pr, &json!([]));
    let args = json!({"repo":cx.repo_arg,"pr":cx.pr,"attempt":"all-1","wait_for_all":true});
    let resp = call_tool(&cx.fx, 60, PR_CHECKS_TOOL, args.clone()).await;
    let op_id = assert_receipt(&resp);
    let _child = parked_process_group_guard(&cx.fx.repo, &op_id).await;
    cx.assert_still_waiting(&op_id).await;
    seed_shim_pr_checks(
        &cx.fx.origin_repo,
        cx.pr,
        &json!([action_check(1, "COMPLETED"), action_check(2, "IN_PROGRESS")]),
    );
    sleep(Duration::from_secs(16)).await;
    cx.assert_still_waiting(&op_id).await;
    let log = std::fs::read_to_string(shim_state_dir(&cx.fx.origin_repo).join("gh.log")).unwrap();
    assert!(!log.contains("actions/runs"), "poll must not enrich: {log}");
    seed_shim_pr_checks(
        &cx.fx.origin_repo,
        cx.pr,
        &json!([action_check(1, "COMPLETED"), action_check(2, "COMPLETED")]),
    );
    let event = cx.wait_for_event(Duration::from_secs(35)).await;
    assert_eq!(event.payload["snapshot"]["all_checks_completed"], true);
    let failed = event.payload["failed_checks"].as_array().unwrap();
    assert_eq!(failed.len(), 2);
    for (index, check) in failed.iter().enumerate() {
        let shard = index + 1;
        let diagnostics = &check["diagnostics"];
        assert_eq!(diagnostics["status"], "available", "{check}");
        assert_eq!(
            diagnostics["failed_steps"],
            json!([format!("test shard {shard}")])
        );
        let annotation = junit_annotation(shard as u64);
        let test_name = junit_test_name(shard);
        assert_eq!(diagnostics["failed_tests"], json!([test_name]));
        assert_eq!(diagnostics["error_summary"], annotation["message"]);
        assert!(checks_wake(&event.payload).contains(test_name));
        assert!(checks_wake(&event.payload).contains(annotation["message"].as_str().unwrap()));
        assert!(checks_wake(&event.payload).contains(diagnostics["log_url"].as_str().unwrap()));
        assert_eq!(
            diagnostics["log_url"],
            format!(
                "https://github.com/{}/actions/runs/100/job/{}",
                cx.repo_arg,
                500 + shard
            )
        );
    }
    let replay = call_tool(&cx.fx, 61, PR_CHECKS_TOOL, args).await;
    let replayed = calm_types::event::Event::from_kind_and_payload(
        "forge.pr.checks",
        replay["result"]["structuredContent"]["result"]["event"].clone(),
    )
    .unwrap();
    assert_eq!(replayed.payload_value(), event.payload);
    let log = std::fs::read_to_string(shim_state_dir(&cx.fx.origin_repo).join("gh.log")).unwrap();
    assert_eq!(
        log.lines()
            .filter(|s| s.ends_with("/actions/runs/100"))
            .count(),
        4
    );
    assert!(!log.contains("untrusted.example"));
}

#[tokio::test]
async fn gh_pr_checks_job_identity_and_summary_sanitization() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("job-identity").await;
    seed_actions(&cx);
    seed_shim_pr_checks(
        &cx.fx.origin_repo,
        cx.pr,
        &json!([action_check(1, "COMPLETED"), action_check(2, "COMPLETED")]),
    );
    let mut wrong = jobs(&cx);
    wrong["jobs"][0]["check_run_url"] = json!(format!(
        "https://evil.example/repos/{}/check-runs/1",
        cx.repo_arg
    ));
    api_fixture(&cx, "actions/runs/100/attempts/2/jobs?per_page=100", wrong);
    let result = probe(&cx);
    assert_eq!(result["failed_checks"].as_array().unwrap().len(), 2);
    assert_eq!(
        result["failed_checks"][0]["diagnostics"]["status"],
        "unavailable"
    );
    assert_eq!(
        result["failed_checks"][1]["diagnostics"]["status"],
        "available"
    );
    for (field, value) in [
        ("run_id", json!(101)),
        ("run_attempt", json!(1)),
        ("head_sha", json!("another-head")),
        ("run_url", json!("https://evil.example/run")),
        ("status", json!("in_progress")),
        ("steps", json!([{"conclusion":"failure","name":null}])),
    ] {
        let mut wrong = jobs(&cx);
        wrong["jobs"][0][field] = value;
        api_fixture(&cx, "actions/runs/100/attempts/2/jobs?per_page=100", wrong);
        let result = probe(&cx);
        assert_eq!(
            result["failed_checks"][0]["diagnostics"]["status"], "unavailable",
            "{field}: {result}"
        );
        assert_eq!(
            result["failed_checks"][1]["diagnostics"]["status"], "available",
            "{field}: {result}"
        );
    }
    seed_actions(&cx);
    api_fixture(
        &cx,
        "check-runs/1",
        json!({
            "id":1,"node_id":"CheckRun-0", "head_sha":cx.head_sha(),"check_suite":{"id":90},"status":"completed",
            "app":{"slug":"github-actions"},"output":{"summary":format!("\u{1b}[31massert failed\u{1b}[0m\u{0}\u{202e} token=hidden ghp_123secret Authorization: Bearer injected https://logs.example/x?sig=hidden {}","x".repeat(4000))}
        }),
    );
    let result = probe(&cx);
    let diagnostics = &result["failed_checks"][0]["diagnostics"];
    assert_eq!(diagnostics["status"], "available", "{result}");
    let summary = diagnostics["error_summary"].as_str().unwrap();
    assert!(summary.starts_with("assert failed"), "{summary}");
    assert!(summary.len() <= 1000);
    for secret in [
        "hidden",
        "ghp_123secret",
        "injected",
        "\u{1b}",
        "\u{0}",
        "\u{202e}",
        "sig=",
    ] {
        assert!(!summary.contains(secret), "{summary}");
    }
    assert_eq!(diagnostics["truncated"], true);
    api_fixture(
        &cx,
        "actions/runs/100",
        json!({
            "id":100,"head_sha":"another-head","run_attempt":2,"check_suite_id":90,"repository":{"full_name":cx.repo_arg}
        }),
    );
    let result = probe(&cx);
    assert!(
        result["failed_checks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["diagnostics"]["status"] == "unavailable")
    );
}

#[tokio::test]
async fn gh_pr_checks_enrichment_bounds_paginated_jobs_and_annotations() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("bounded-pages").await;
    seed_actions(&cx);
    seed_shim_pr_checks(
        &cx.fx.origin_repo,
        cx.pr,
        &json!([action_check(1, "COMPLETED")]),
    );
    let dir = shim_state_dir(&cx.fx.origin_repo).join("actions");
    let endpoint = format!(
        "repos/{}/actions/runs/100/attempts/2/jobs?per_page=100",
        cx.repo_arg
    );
    // Valid raw API pages with no matching jobs, then the matching final page. The
    // production reader must refuse the entire oversized stream, not use its tail.
    let mut unrelated = jobs(&cx);
    for job in unrelated["jobs"].as_array_mut().unwrap() {
        job["check_run_url"] = json!("https://api.github.com/repos/other/repo/check-runs/999");
    }
    std::fs::write(
        dir.join(endpoint.replace('/', "_")),
        format!("{}{}\n", format!("{unrelated}\n").repeat(2000), jobs(&cx)),
    )
    .unwrap();
    let result = probe(&cx);
    assert_eq!(
        result["failed_checks"][0]["diagnostics"]["status"],
        "unavailable"
    );
    assert_eq!(
        result["failed_checks"][0]["diagnostics"]["reason"],
        "Actions jobs unavailable"
    );

    seed_actions(&cx);
    api_fixture(
        &cx,
        "check-runs/1",
        json!({
            "id":1,"node_id":"CheckRun-0","head_sha":cx.head_sha(),"check_suite":{"id":90},
            "status":"completed","app":{"slug":"github-actions"},"output":{"summary":""}
        }),
    );
    let endpoint = format!(
        "repos/{}/check-runs/1/annotations?per_page=100",
        cx.repo_arg
    );
    let page = json!([{"annotation_level":"failure","message":"x".repeat(1000)}]);
    std::fs::write(
        dir.join(endpoint.replace('/', "_")),
        format!("{page}\n").repeat(1000),
    )
    .unwrap();
    let result = probe(&cx);
    assert_eq!(result["conclusion"], "failure");
    assert_eq!(result["failed_checks"].as_array().unwrap().len(), 1);
    assert_eq!(
        result["failed_checks"][0]["diagnostics"]["status"],
        "unavailable"
    );
    assert_eq!(
        result["failed_checks"][0]["diagnostics"]["reason"],
        "check annotations unavailable"
    );
}

#[tokio::test]
async fn gh_pr_checks_unknown_test_provider_keeps_steps_and_marks_tests_uncollected() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("unknown-tests").await;
    seed_actions(&cx);
    seed_shim_pr_checks(
        &cx.fx.origin_repo,
        cx.pr,
        &json!([action_check(1, "COMPLETED")]),
    );
    api_fixture(
        &cx,
        "check-runs/1/annotations?per_page=100",
        json!([
            {"annotation_level":"failure","title":"nextest failure-ish: invented::test","message":"unknown runner"},
            {"annotation_level":"failure","title":"nextest failure: missing-binary","message":"missing identity"},
            {"annotation_level":"warning","title":"nextest failure: ignored::warning","message":"not a failure"}
        ]),
    );
    let result = probe(&cx);
    let diagnostics = &result["failed_checks"][0]["diagnostics"];
    assert_eq!(diagnostics["status"], "available");
    assert_eq!(diagnostics["failed_tests"], json!([]));
    assert_eq!(diagnostics["failed_steps"], json!(["test shard 1"]));
    let payload = json!({"track_id":cx.fx.track_id,"pr_number":cx.pr,"conclusion":result["conclusion"],"snapshot":result["snapshot"],"failed_checks":result["failed_checks"]});
    let wake = checks_wake(&payload);
    assert!(wake.contains("tests=not collected"), "{wake}");
    assert!(wake.contains("test shard 1"));
}

#[tokio::test]
async fn gh_pr_checks_partial_actions_api_failure_keeps_failure_collection() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("partial-api").await;
    seed_actions(&cx);
    seed_shim_pr_checks(
        &cx.fx.origin_repo,
        cx.pr,
        &json!([action_check(1, "COMPLETED"), action_check(2, "COMPLETED")]),
    );
    let endpoint = format!(
        "repos/{}/actions/runs/100/attempts/2/jobs?per_page=100",
        cx.repo_arg
    );
    std::fs::write(
        shim_state_dir(&cx.fx.origin_repo)
            .join("actions")
            .join(format!("{}.exit_status", endpoint.replace('/', "_"))),
        "1",
    )
    .unwrap();
    let result = probe(&cx);
    assert_eq!(result["conclusion"], "failure");
    assert_eq!(result["snapshot"]["all_checks_completed"], true);
    let failed = result["failed_checks"].as_array().unwrap();
    assert_eq!(failed.len(), 2);
    for check in failed {
        assert_eq!(check["diagnostics"]["status"], "unavailable");
        assert_eq!(check["diagnostics"]["reason"], "Actions jobs unavailable");
    }
}

#[tokio::test]
async fn gh_pr_checks_enrichment_rechecks_head_and_paginates_jobs_and_annotations() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("pages-and-head").await;
    seed_actions(&cx);
    seed_shim_pr_checks(
        &cx.fx.origin_repo,
        cx.pr,
        &json!([action_check(1, "COMPLETED"), action_check(2, "COMPLETED")]),
    );
    let dir = shim_state_dir(&cx.fx.origin_repo).join("actions");
    let endpoint = format!(
        "repos/{}/actions/runs/100/attempts/2/jobs?per_page=100",
        cx.repo_arg
    );
    std::fs::write(
        dir.join(endpoint.replace('/', "_")),
        format!("{}\n{}\n", json!({"jobs":[]}), jobs(&cx)),
    )
    .unwrap();
    api_fixture(
        &cx,
        "check-runs/1",
        json!({
            "id":1,"node_id":"CheckRun-0","head_sha":cx.head_sha(),"check_suite":{"id":90},"status":"completed",
            "app":{"slug":"github-actions"},"output":{"summary":"","text":""}
        }),
    );
    let endpoint = format!(
        "repos/{}/check-runs/1/annotations?per_page=100",
        cx.repo_arg
    );
    std::fs::write(
        dir.join(endpoint.replace('/', "_")),
        format!(
            "[]\n{}\n",
            json!([{"annotation_level":"failure","message":"error on second page"}])
        ),
    )
    .unwrap();
    let result = probe(&cx);
    assert_eq!(
        result["failed_checks"][0]["diagnostics"]["status"], "available",
        "{result}"
    );
    assert_eq!(
        result["failed_checks"][0]["diagnostics"]["error_summary"],
        "error on second page"
    );
    let state = shim_state_dir(&cx.fx.origin_repo);
    std::fs::write(state.join("block_api"), "").unwrap();
    let payload = lower("gh_pr_checks", &json!({"repo":cx.repo_arg,"pr":cx.pr})).unwrap();
    let read: Vec<String> =
        serde_json::from_value(payload["probe"]["output_probe_argv"].clone()).unwrap();
    let gh = cx._env._path_dir.path().join("gh");
    let before = std::fs::read_to_string(state.join("gh.log"))
        .unwrap()
        .lines()
        .count();
    let running = tokio::task::spawn_blocking(move || run_checks_read(&gh, &read));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let log = std::fs::read_to_string(state.join("gh.log")).unwrap();
        if log
            .lines()
            .skip(before)
            .any(|s| s.starts_with("api repos/"))
        {
            break;
        }
        assert!(Instant::now() < deadline, "enrichment did not start");
        sleep(Duration::from_millis(25)).await;
    }
    cx.commit_to_pr_branch();
    std::fs::write(state.join("release_api"), "").unwrap();
    let output = running.await.unwrap();
    assert!(
        !output.status.success(),
        "head moved during enrichment: {output:?}"
    );
    assert!(output.stdout.is_empty(), "stale evidence must not escape");
}
