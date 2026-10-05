//! #1965, #2058: `gh_pr_checks` folds gh's rollup with its real `--jq` filter, then waits on the
//! PR's current head and wakes the Planner when the checks settle, the PR conflicts, the head
//! moves, or the parked deadline passes.
use super::*;
use crate::support::gh_shim::{run_gh, seed_shim_pr_checks, seed_shim_pr_mergeable};
use crate::support::git_helpers::git_stdout_no_cwd;
use calm_server::builtin_plugins::dev::git_actions::lower;
use std::collections::BTreeSet;

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

fn success(name: &str) -> Value {
    check_run(name, "COMPLETED", "SUCCESS")
}

fn pending_rollup() -> Value {
    json!([success("lint"), check_run("shard", "IN_PROGRESS", "")])
}

fn run_checks_read(gh: &std::path::Path, read: &[String]) -> std::process::Output {
    std::process::Command::new(&read[0])
        .args(&read[1..])
        .env(
            "PATH",
            format!(
                "{}:{}",
                gh.parent().unwrap().display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .output()
        .expect("run production checks output probe")
}

/// The `failed-run` row: one check failed while another still runs.
fn failed_while_running_rollup() -> Value {
    json!([
        check_run("shard", "IN_PROGRESS", ""),
        check_run("test", "COMPLETED", "FAILURE")
    ])
}

#[test]
fn gh_pr_checks_reports_no_checks_and_mergeability() {
    let bin = short_tempdir("p").expect("gh shim PATH tempdir");
    write_gh_shim(bin.path());
    let gh = bin.path().join("gh");
    let seed = track_cwd_tempdir("c").expect("checks repo");
    let repo = seed.path().join(".git");
    let repo_arg = repo.display().to_string();
    let created = run_gh(
        &gh,
        &[
            "pr", "create", "--repo", &repo_arg, "--head", "main", "--base", "main",
        ],
    );
    assert!(created.status.success(), "{created:?}");
    let created: Value = serde_json::from_slice(&created.stdout).expect("pr create json");
    let pr_number = created["number"].as_u64().expect("pr number");
    let head_sha = run_git_capture(seed.path(), ["rev-parse", "HEAD"]);

    // The deadline snapshot is the lowered output probe: one read, never the waiting script.
    let payload = lower(
        "gh_pr_checks",
        &json!({ "repo": repo_arg, "pr": pr_number, "attempt": "t1" }),
    )
    .expect("lower gh_pr_checks");
    let read: Vec<String> = serde_json::from_value(payload["probe"]["output_probe_argv"].clone())
        .expect("gh_pr_checks output probe argv");
    assert_eq!(
        read[0], "sh",
        "the output probe validates a paginated read: {read:?}"
    );

    let cases = [
        // PR #1964's shape while its Rust shards ran.
        (
            "in-progress-run",
            pending_rollup(),
            "MERGEABLE",
            "pending",
            "mergeable",
        ),
        (
            "queued-run",
            json!([success("lint"), check_run("shard", "QUEUED", "")]),
            "MERGEABLE",
            "pending",
            "mergeable",
        ),
        (
            "empty-rollup",
            json!([]),
            "MERGEABLE",
            "no_checks",
            "mergeable",
        ),
        (
            "null-rollup",
            Value::Null,
            "MERGEABLE",
            "no_checks",
            "mergeable",
        ),
        // PR #2042: conflicting with main, so no pull_request workflow ran.
        (
            "conflicting-no-checks",
            json!([]),
            "CONFLICTING",
            "no_checks",
            "conflicting",
        ),
        (
            "unknown-no-checks",
            Value::Null,
            "UNKNOWN",
            "no_checks",
            "unknown",
        ),
        (
            "pending-status",
            json!([success("lint"), status_context("ci/legacy", "PENDING")]),
            "MERGEABLE",
            "pending",
            "mergeable",
        ),
        (
            "failed-run",
            failed_while_running_rollup(),
            "MERGEABLE",
            "failure",
            "mergeable",
        ),
        (
            "cancelled-run",
            json!([success("lint"), check_run("test", "COMPLETED", "CANCELLED")]),
            "MERGEABLE",
            "failure",
            "mergeable",
        ),
        (
            "action-required-run",
            json!([check_run("deploy", "COMPLETED", "ACTION_REQUIRED")]),
            "MERGEABLE",
            "failure",
            "mergeable",
        ),
        (
            "startup-failure-run",
            json!([check_run("test", "COMPLETED", "STARTUP_FAILURE")]),
            "MERGEABLE",
            "failure",
            "mergeable",
        ),
        (
            "error-status",
            json!([success("lint"), status_context("ci/legacy", "ERROR")]),
            "MERGEABLE",
            "failure",
            "mergeable",
        ),
        (
            "id-only-run",
            json!([{
                "__typename": "CheckRun", "name": "test", "status": "COMPLETED",
                "conclusion": "TIMED_OUT", "detailsUrl": ""
            }]),
            "MERGEABLE",
            "failure",
            "mergeable",
        ),
        (
            "id-only-status",
            json!([{
                "__typename": "StatusContext", "context": "ci/legacy", "state": "FAILURE",
                "targetUrl": ""
            }]),
            "MERGEABLE",
            "failure",
            "mergeable",
        ),
        (
            "all-finished-green",
            json!([
                success("lint"),
                check_run("docs", "COMPLETED", "SKIPPED"),
                check_run("advisory", "COMPLETED", "NEUTRAL"),
                status_context("ci/legacy", "SUCCESS")
            ]),
            "MERGEABLE",
            "success",
            "mergeable",
        ),
        // PR #2024: conflicting with every check finished.
        (
            "conflicting-green",
            json!([success("lint")]),
            "CONFLICTING",
            "success",
            "conflicting",
        ),
    ];

    let mut mismatches = Vec::new();
    for (case, rollup, mergeable, conclusion, folded_mergeable) in &cases {
        seed_shim_pr_checks(&repo, pr_number, rollup);
        seed_shim_pr_mergeable(&repo, pr_number, mergeable);
        let output = run_checks_read(&gh, &read);
        let want = json!({
            "conclusion": conclusion,
            "mergeable": folded_mergeable,
            "head_sha": head_sha,
            "snapshot": { "head_sha": head_sha, "mergeable": folded_mergeable },
            "failed_checks": match *case {
                "failed-run" | "cancelled-run" | "startup-failure-run" => json!([
                    {"name": "test", "url": "https://github.invalid/shim/checks/1"}
                ]),
                "action-required-run" => json!([
                    {"name": "deploy", "url": "https://github.invalid/shim/checks/1"}
                ]),
                "error-status" => json!([
                    {"name": "ci/legacy", "url": "https://github.invalid/shim/status/1"}
                ]),
                "id-only-run" => json!([{ "name": "test", "id": "CheckRun-0" }]),
                "id-only-status" => json!([{ "name": "ci/legacy", "id": "StatusContext-0" }]),
                _ => json!([]),
            }
        });
        match serde_json::from_slice::<Value>(&output.stdout) {
            Ok(got) if output.status.success() && got == want => {}
            got => mismatches.push(format!("{case}: expected {want}, got {got:?} ({output:?})")),
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");

    // Failures beyond page one must not disappear. Seed only the real export's fields;
    // the GraphQL shim models IDs on the source nodes separately from the lossy exporter.
    let mut checks = vec![success("passing"); 100];
    checks.push(json!({"__typename":"CheckRun", "name":"last-run", "status":"COMPLETED", "conclusion":"FAILURE", "detailsUrl":""}));
    checks.push(json!({"__typename":"StatusContext", "context":"last-status", "state":"FAILURE", "targetUrl":""}));
    seed_shim_pr_checks(&repo, pr_number, &json!(checks));
    let output = run_checks_read(&gh, &read);
    assert!(output.status.success(), "{output:?}");
    let got: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        got["failed_checks"],
        json!([
            {"name":"last-run", "id":"CheckRun-100"},
            {"name":"last-status", "id":"StatusContext-101"}
        ])
    );

    let page = |head: &str, oid: &str| {
        json!({"data":{"repository":{"pullRequest":{
            "headRefOid":head, "mergeable":"MERGEABLE", "commits":{"nodes":[{"commit":{
                "oid":oid, "statusCheckRollup":{"contexts":{"nodes":[success("test")],
                    "pageInfo":{"hasNextPage":false,"endCursor":null}}}
            }}]}
        }}}})
    };
    let pages_path = shim_state_dir(&repo)
        .join("checks")
        .join(format!("{pr_number}.pages"));
    for pages in [
        json!([page(&head_sha, &head_sha), page("moved", "moved")]),
        json!([page(&head_sha, "wrong-commit")]),
        json!([page("stale", "stale")]),
    ] {
        std::fs::write(&pages_path, pages.to_string()).unwrap();
        let output = run_checks_read(&gh, &read);
        assert!(
            !output.status.success(),
            "mixed/stale snapshot must fail: {output:?}"
        );
        assert!(
            output.stdout.is_empty(),
            "must not emit mixed evidence: {output:?}"
        );
    }
}

#[tokio::test]
async fn a_conflicting_pr_returns_at_once() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("conflicting").await;
    seed_shim_pr_checks(&cx.fx.origin_repo, cx.pr, &json!([]));
    seed_shim_pr_mergeable(&cx.fx.origin_repo, cx.pr, "CONFLICTING");

    let resp = cx.call(41, "b-1").await;
    let op_id = assert_receipt_or_settled(&resp);
    let _child = parked_process_group_guard(&cx.fx.repo, &op_id).await;
    let event = cx.wait_for_event(Duration::from_secs(10)).await;
    assert_eq!(event.payload["conclusion"], "no_checks", "{event:?}");

    let repeated = cx.call(42, "b-1").await;
    assert_eq!(
        repeated["result"]["structuredContent"]["result"]["event"],
        cx.recorded_event("no_checks", "conflicting", &cx.head_sha(), json!([])),
        "{repeated}"
    );
}

#[tokio::test]
async fn a_checks_wait_parks_until_ci_fails() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("fails").await;
    seed_shim_pr_checks(&cx.fx.origin_repo, cx.pr, &pending_rollup());

    let resp = cx.call(43, "a-1").await;
    let op_id = assert_receipt(&resp);
    let _child = parked_process_group_guard(&cx.fx.repo, &op_id).await;
    cx.assert_still_waiting(&op_id).await;

    let pending_again = cx.call(49, "a-1").await;
    assert_eq!(
        pending_again["result"]["structuredContent"],
        resp["result"]["structuredContent"]
    );

    seed_shim_pr_checks(
        &cx.fx.origin_repo,
        cx.pr,
        &json!([
            check_run("test", "COMPLETED", "FAILURE"),
            status_context("ci/legacy", "ERROR"),
            check_run("shard", "IN_PROGRESS", "")
        ]),
    );
    let event = cx.wait_for_event(Duration::from_secs(35)).await;
    assert_eq!(event.payload["snapshot"]["head_sha"], cx.head_sha());
    assert_eq!(event.payload["snapshot"]["mergeable"], "mergeable");
    // #2170: the persisted event keeps the failed checks the wake and scorecard read.
    assert_eq!(
        event.payload,
        json!({ "track_id": cx.fx.track_id, "pr_number": cx.pr, "conclusion": "failure",
            "snapshot": { "head_sha": cx.head_sha(), "mergeable": "mergeable" },
            "failed_checks": [
                { "name": "test", "url": "https://github.invalid/shim/checks/1" },
                { "name": "ci/legacy", "url": "https://github.invalid/shim/status/1" }
            ] })
    );
    assert_track_event(&event, &cx.fx.track_id);

    let repeated = cx.call(44, "a-1").await;
    assert_eq!(
        repeated["result"]["structuredContent"]["result"]["event"]["failed_checks"],
        json!([
            { "name": "test", "url": "https://github.invalid/shim/checks/1" },
            { "name": "ci/legacy", "url": "https://github.invalid/shim/status/1" }
        ])
    );
    assert_eq!(
        repeated["result"]["structuredContent"]["result"]["event"],
        cx.recorded_event(
            "failure",
            "mergeable",
            &cx.head_sha(),
            json!([
                { "name": "test", "url": "https://github.invalid/shim/checks/1" },
                { "name": "ci/legacy", "url": "https://github.invalid/shim/status/1" }
            ])
        ),
        "{repeated}"
    );
}

#[tokio::test]
async fn a_moved_head_ends_the_wait() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    assert!(
        std::env::var_os("NEIGE_FORGE_DEADLINE_SECS").is_none(),
        "this test relies on the default parked deadline"
    );
    let cx = ChecksFixture::boot("moves").await;
    seed_shim_pr_checks(&cx.fx.origin_repo, cx.pr, &pending_rollup());
    let first_head = cx.head_sha();

    let resp = cx.call(45, "h-1").await;
    let op_id = assert_receipt(&resp);
    let _child = parked_process_group_guard(&cx.fx.repo, &op_id).await;
    cx.assert_still_waiting(&op_id).await;

    let moved = cx.commit_to_pr_branch();
    assert_ne!(moved, first_head);
    let event = cx.wait_for_event(Duration::from_secs(35)).await;
    assert_eq!(event.payload["conclusion"], "pending", "{event:?}");

    assert_eq!(event.payload["snapshot"]["head_sha"], moved);
    let repeated = cx.call(46, "h-1").await;
    assert_eq!(
        repeated["result"]["structuredContent"]["result"]["event"],
        cx.recorded_event("pending", "mergeable", &moved, json!([])),
        "{repeated}"
    );
}

#[tokio::test]
async fn a_wait_past_its_deadline_wakes_with_the_current_state() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let _deadline = EnvGuard::set("NEIGE_FORGE_DEADLINE_SECS", "1");
    let cx = ChecksFixture::boot("deadline").await;
    seed_shim_pr_checks(&cx.fx.origin_repo, cx.pr, &pending_rollup());

    let resp = cx.call(47, "c-1").await;
    let op_id = assert_receipt(&resp);
    let _child = parked_process_group_guard(&cx.fx.repo, &op_id).await;
    // Past the 1 s deadline, but only the sweep may end the wait.
    cx.assert_still_waiting(&op_id).await;

    cx.fx._runtime.sweep_parked().await.expect("parked sweep");
    assert_eq!(operation_phase(&cx.fx.repo, &op_id).await, "succeeded");
    let event = cx.wait_for_event(Duration::from_secs(5)).await;
    assert_eq!(event.payload["conclusion"], "pending", "{event:?}");
}

#[tokio::test]
async fn a_failed_check_ends_the_wait_while_others_run() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("fail-fast").await;
    seed_shim_pr_checks(&cx.fx.origin_repo, cx.pr, &failed_while_running_rollup());

    let resp = cx.call(48, "f-1").await;
    let op_id = assert_receipt_or_settled(&resp);
    let _child = parked_process_group_guard(&cx.fx.repo, &op_id).await;
    let event = cx.wait_for_event(Duration::from_secs(10)).await;
    assert_eq!(event.payload["conclusion"], "failure", "{event:?}");
}

#[tokio::test]
async fn a_successful_checks_wait_records_exact_head() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = ChecksFixture::boot("success").await;
    seed_shim_pr_checks(&cx.fx.origin_repo, cx.pr, &json!([success("lint")]));
    let resp = cx.call(50, "s-1").await;
    let op_id = assert_receipt_or_settled(&resp);
    let _child = parked_process_group_guard(&cx.fx.repo, &op_id).await;
    let event = cx.wait_for_event(Duration::from_secs(10)).await;
    assert_eq!(event.payload["conclusion"], "success");
    assert_eq!(event.payload["snapshot"]["head_sha"], cx.head_sha());
    assert_eq!(event.payload["snapshot"]["mergeable"], "mergeable");
    assert_eq!(event.payload["failed_checks"], json!([]));
    let repeated = cx.call(51, "s-1").await;
    assert_eq!(
        repeated["result"]["structuredContent"]["result"]["event"]["failed_checks"],
        json!([])
    );
}

/// A track fixture with an open PR on its own origin branch, read through the production MCP path.
struct ChecksFixture {
    fx: Fixture,
    repo_arg: String,
    branch: String,
    pr: u64,
    _env: ForgeTestEnv,
}

impl ChecksFixture {
    async fn boot(branch: &str) -> Self {
        let env = setup_forge_env();
        let fx = boot_fixture().await;
        let repo_arg = fx.origin_repo.display().to_string();
        let branch = format!("checks-{branch}");
        git_stdout_no_cwd(["--git-dir", &repo_arg, "branch", &branch, "main"]);
        let created = run_gh(
            &env._path_dir.path().join("gh"),
            &[
                "pr", "create", "--repo", &repo_arg, "--head", &branch, "--base", "main",
            ],
        );
        assert!(created.status.success(), "{created:?}");
        let created: Value = serde_json::from_slice(&created.stdout).expect("pr create json");
        let pr = created["number"].as_u64().expect("pr number");
        Self {
            fx,
            repo_arg,
            branch,
            pr,
            _env: env,
        }
    }

    async fn call(&self, id: i64, attempt: &str) -> Value {
        call_tool(
            &self.fx,
            id,
            PR_CHECKS_TOOL,
            json!({ "repo": self.repo_arg, "pr": self.pr, "attempt": attempt }),
        )
        .await
    }

    fn head_sha(&self) -> String {
        git_stdout_no_cwd(["--git-dir", &self.repo_arg, "rev-parse", &self.branch])
    }

    /// A new commit on the PR branch, as a push would land it.
    fn commit_to_pr_branch(&self) -> String {
        let tree = format!("{}^{{tree}}", self.branch);
        let tree = git_stdout_no_cwd(["--git-dir", &self.repo_arg, "rev-parse", &tree]);
        let commit = git_stdout_no_cwd([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.test",
            "--git-dir",
            &self.repo_arg,
            "commit-tree",
            &tree,
            "-p",
            &self.branch,
            "-m",
            "move the PR head",
        ]);
        let branch_ref = format!("refs/heads/{}", self.branch);
        git_stdout_no_cwd([
            "--git-dir",
            &self.repo_arg,
            "update-ref",
            &branch_ref,
            &commit,
        ]);
        commit
    }

    fn reads(&self) -> usize {
        let read = format!("api graphql pr={}", self.pr);
        std::fs::read_to_string(shim_state_dir(&self.fx.origin_repo).join("gh.log"))
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains(&read))
            .count()
    }

    /// After the first read and 2 s more (under one poll interval): parked, and no event yet.
    async fn assert_still_waiting(&self, op_id: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.reads() == 0 {
            assert!(Instant::now() < deadline, "the wait never read the PR");
            sleep(Duration::from_millis(25)).await;
        }
        sleep(Duration::from_secs(2)).await;
        assert_eq!(operation_phase(&self.fx.repo, op_id).await, "parked");
        assert!(
            self.events().await.is_empty(),
            "an unsettled wait must not wake the Planner: {:?}",
            self.events().await
        );
    }

    async fn events(&self) -> Vec<EventRow> {
        event_rows(&self.fx.repo, "forge.pr.checks")
            .await
            .into_iter()
            .filter(|row| row.payload["pr_number"] == json!(self.pr))
            .collect()
    }

    async fn wait_for_event(&self, within: Duration) -> EventRow {
        let deadline = Instant::now() + within;
        loop {
            let rows = self.events().await;
            if let Some(row) = rows.first() {
                assert_eq!(rows.len(), 1, "one wait wakes once: {rows:?}");
                return row.clone();
            }
            assert!(
                Instant::now() < deadline,
                "no forge.pr.checks event within {within:?}"
            );
            sleep(Duration::from_millis(25)).await;
        }
    }

    fn recorded_event(
        &self,
        conclusion: &str,
        mergeable: &str,
        head_sha: &str,
        failed_checks: Value,
    ) -> Value {
        json!({
            "track_id": self.fx.track_id,
            "pr_number": self.pr,
            "conclusion": conclusion,
            "mergeable": mergeable,
            "head_sha": head_sha,
            "snapshot": { "head_sha": head_sha, "mergeable": mergeable },
            "failed_checks": failed_checks
        })
    }
}

/// The standard parked-forge receipt, shared by every forge tool.
fn assert_receipt(resp: &Value) -> String {
    assert_tool_succeeded(resp, "gh_pr_checks");
    let receipt = &resp["result"]["structuredContent"];
    let keys: BTreeSet<&str> = receipt
        .as_object()
        .expect("receipt object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from(["completion_event", "message", "op_id", "parked", "status"]),
        "{resp}"
    );
    assert_eq!(receipt["parked"], true, "{resp}");
    assert_eq!(receipt["status"], "pending", "{resp}");
    assert_eq!(receipt["completion_event"], "forge.pr.checks", "{resp}");
    receipt["op_id"]
        .as_str()
        .expect("receipt op_id")
        .to_string()
}

/// A wait whose first read settles can finish before the transport looks the op up
/// (`transport.rs`, the parked branch); that call returns the recorded result instead of the
/// receipt, and the event still wakes once. Either answer is valid here.
pub(super) fn assert_receipt_or_settled(resp: &Value) -> String {
    let answer = &resp["result"]["structuredContent"];
    if answer["parked"] == json!(true) {
        return assert_receipt(resp);
    }
    assert_tool_succeeded(resp, "gh_pr_checks");
    assert_eq!(answer["result"]["event_kind"], "forge.pr.checks", "{resp}");
    answer["op_id"].as_str().expect("result op_id").to_string()
}
