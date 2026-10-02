//! #1965: `gh.pr.checks` runs its real `--jq` filter over gh-export-shaped rollups and reports
//! pending until every check has finished.
use super::*;
use crate::support::gh_shim::{run_gh, seed_shim_pr_checks};

/// One CheckRun as `gh pr view --json statusCheckRollup` exports it. An unfinished run carries
/// `conclusion: ""` and a zero `completedAt`.
fn check_run(name: &str, status: &str, conclusion: &str) -> Value {
    let completed_at = if status == "COMPLETED" {
        "2026-10-02T15:20:00Z"
    } else {
        "0001-01-01T00:00:00Z"
    };
    json!({
        "__typename": "CheckRun",
        "name": name,
        "status": status,
        "conclusion": conclusion,
        "startedAt": "2026-10-02T15:13:19Z",
        "completedAt": completed_at,
        "detailsUrl": "https://github.invalid/shim/checks/1"
    })
}

/// One commit status as gh exports it: no `status` or `conclusion`, only `state`.
fn status_context(context: &str, state: &str) -> Value {
    json!({
        "__typename": "StatusContext",
        "context": context,
        "state": state,
        "targetUrl": "https://github.invalid/shim/status/1",
        "startedAt": "2026-10-02T15:13:19Z"
    })
}

#[tokio::test]
async fn gh_pr_checks_reports_pending_until_every_check_finishes() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let path_dir = short_tempdir("p").expect("gh shim PATH tempdir");
    write_gh_shim(path_dir.path());
    let path_value = prepend_to_path(path_dir.path());
    let results_dir = short_tempdir("r").expect("forge results tempdir");
    let _trusted = EnvGuard::set("NEIGE_TRUSTED_FORGE_PLUGINS", PLUGIN_ID);
    let _results = EnvGuard::set("NEIGE_FORGE_RESULTS_DIR", results_dir.path());
    let _path = EnvGuard::set("PATH", path_value);

    let fx = boot_fixture().await;
    let repo_arg = fx.origin_repo.display().to_string();
    let created = run_gh(
        &path_dir.path().join("gh"),
        &[
            "pr", "create", "--repo", &repo_arg, "--head", "main", "--base", "main",
        ],
    );
    assert!(created.status.success(), "{created:?}");
    let created: Value = serde_json::from_slice(&created.stdout).expect("pr create json");
    let pr_number = created["number"].as_u64().expect("pr number");

    let success = |name| check_run(name, "COMPLETED", "SUCCESS");
    let cases = [
        // PR #1964's shape while its Rust shards ran.
        (
            "in-progress-run",
            json!([success("lint"), check_run("shard", "IN_PROGRESS", "")]),
            "pending",
        ),
        (
            "queued-run",
            json!([success("lint"), check_run("shard", "QUEUED", "")]),
            "pending",
        ),
        ("empty-rollup", json!([]), "pending"),
        ("null-rollup", Value::Null, "pending"),
        (
            "pending-status",
            json!([success("lint"), status_context("ci/legacy", "PENDING")]),
            "pending",
        ),
        (
            "failed-run",
            json!([
                check_run("shard", "IN_PROGRESS", ""),
                check_run("test", "COMPLETED", "FAILURE")
            ]),
            "failure",
        ),
        (
            "cancelled-run",
            json!([success("lint"), check_run("test", "COMPLETED", "CANCELLED")]),
            "failure",
        ),
        (
            "action-required-run",
            json!([check_run("deploy", "COMPLETED", "ACTION_REQUIRED")]),
            "failure",
        ),
        (
            "startup-failure-run",
            json!([check_run("test", "COMPLETED", "STARTUP_FAILURE")]),
            "failure",
        ),
        (
            "error-status",
            json!([success("lint"), status_context("ci/legacy", "ERROR")]),
            "failure",
        ),
        (
            "all-finished-green",
            json!([
                success("lint"),
                check_run("docs", "COMPLETED", "SKIPPED"),
                check_run("advisory", "COMPLETED", "NEUTRAL"),
                status_context("ci/legacy", "SUCCESS")
            ]),
            "success",
        ),
    ];

    let mut mismatches = Vec::new();
    for (id, (attempt, rollup, expected)) in (40..).zip(cases.iter()) {
        seed_shim_pr_checks(&fx.origin_repo, pr_number, rollup);
        let resp = call_tool(
            &fx,
            id,
            PR_CHECKS_TOOL,
            json!({ "repo": repo_arg, "pr": pr_number, "attempt": attempt }),
        )
        .await;
        let conclusion = &resp["result"]["structuredContent"]["result"]["event"]["conclusion"];
        if resp["result"]["isError"] != json!(false) {
            mismatches.push(format!(
                "{attempt}: expected {expected}, tool failed: {resp}"
            ));
        } else if conclusion != expected {
            mismatches.push(format!("{attempt}: expected {expected}, got {conclusion}"));
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");

    let persisted: Vec<Value> = event_rows(&fx.repo, "forge.pr.checks")
        .await
        .iter()
        .map(|row| row.payload["conclusion"].clone())
        .collect();
    let expected: Vec<Value> = cases.iter().map(|(_, _, want)| json!(want)).collect();
    assert_eq!(persisted, expected, "each read persists its own conclusion");
    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
}
