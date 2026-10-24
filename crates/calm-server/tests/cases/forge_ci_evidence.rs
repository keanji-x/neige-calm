//! #2617 item E: `gh_ci_failure_evidence` reads the failed Actions jobs of one exact head with
//! bounded, sanitized log tails. Persisted events reference the artifact; responses may inline it.
use super::*;
use crate::support::git_helpers::git_stdout_no_cwd;

const EVIDENCE_TOOL: &str = "plugin_gitforge_gh_ci_failure_evidence";
const PR: u64 = 7;

/// A track whose origin's `main` is the head under evidence, read through the production MCP path.
struct EvidenceFixture {
    fx: Fixture,
    repo: String,
    head: String,
    _env: ForgeTestEnv,
}

impl EvidenceFixture {
    async fn boot() -> Self {
        let env = setup_forge_env();
        let fx = boot_fixture().await;
        let repo = fx.origin_repo.display().to_string();
        let head = git_stdout_no_cwd(["--git-dir", &repo, "rev-parse", "main"]);
        Self {
            fx,
            repo,
            head,
            _env: env,
        }
    }

    fn write(&self, suffix: &str, bytes: &[u8]) {
        let dir = shim_state_dir(&self.fx.origin_repo).join("actions");
        std::fs::create_dir_all(&dir).unwrap();
        let endpoint = format!("repos/{}/{suffix}", self.repo);
        std::fs::write(dir.join(endpoint.replace('/', "_")), bytes).unwrap();
    }

    fn api(&self, suffix: &str, value: Value) {
        self.write(suffix, value.to_string().as_bytes());
    }

    fn run(&self, id: u64, attempt: u64, conclusion: &str) -> Value {
        json!({
            "id": id, "head_sha": self.head, "run_attempt": attempt, "status": "completed",
            "conclusion": conclusion, "name": format!("CI {id}"),
            "html_url": format!("https://github.com/o/r/actions/runs/{id}"),
            "repository": {"full_name": self.repo}
        })
    }

    /// The head's run listing; each run reads back unchanged unless a test re-seeds it.
    fn seed_runs(&self, runs: &[Value]) {
        self.api(
            &format!("actions/runs?head_sha={}&per_page=100", self.head),
            json!({ "total_count": runs.len(), "workflow_runs": runs }),
        );
        for run in runs {
            self.api(&format!("actions/runs/{}", run["id"]), run.clone());
        }
    }

    fn job(&self, id: u64, run_id: u64, attempt: u64, conclusion: &str) -> Value {
        json!({
            "id": id, "run_id": run_id, "run_attempt": attempt, "head_sha": self.head,
            "name": format!("job {id}"), "status": "completed", "conclusion": conclusion,
            "html_url": format!("https://github.com/o/r/actions/runs/{run_id}/job/{id}"),
            "steps": [
                {"number": 1, "name": "checkout", "conclusion": "success"},
                {"number": 2, "name": format!("test {id}"), "conclusion": "failure"},
                {"number": 3, "name": "upload", "conclusion": "skipped"}
            ]
        })
    }

    fn seed_jobs(&self, run_id: u64, attempt: u64, jobs: &[Value]) {
        self.api(
            &format!("actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100"),
            json!({ "total_count": jobs.len(), "jobs": jobs }),
        );
    }

    fn seed_log(&self, job_id: u64, text: &str) {
        self.write(&format!("actions/jobs/{job_id}/logs"), text.as_bytes());
    }

    async fn call(&self, id: i64, attempt: &str) -> Value {
        call_tool_via_socket(
            &self.fx.socket_path,
            &self.fx.planner_token,
            "planner-thread",
            id,
            EVIDENCE_TOOL,
            json!({ "repo": self.repo, "pr": PR, "head_sha": self.head, "attempt": attempt }),
        )
        .await
    }

    /// The Planner's read: its MCP response, its event and the evidence its artifact holds.
    async fn read(&self, id: i64, attempt: &str) -> (Value, Value, Value) {
        let resp = self.call(id, attempt).await;
        assert_tool_succeeded(&resp, "gh_ci_failure_evidence");
        // Large receipts contain only an overflow reference. Read the event from the
        // recorded operation rather than assuming it is always repeated inline.
        let op_id = op_id_from_response(&resp);
        let op = self
            .fx
            ._runtime
            .operation_on_track(&op_id, FORGE_ACTION_KIND, &self.fx.track_id)
            .await
            .unwrap()
            .expect("recorded evidence operation");
        let event = op.tx_output.expect("settled evidence output").result["event"].clone();
        let artifact = event["artifact_path"].as_str().expect("artifact_path");
        let evidence = serde_json::from_str(&std::fs::read_to_string(artifact).unwrap())
            .expect("evidence artifact is JSON");
        if let Some(reference) =
            resp["result"]["structuredContent"]["result"]["output_ref"].as_str()
        {
            assert_eq!(reference, format!("ops/{op_id}/result"));
            let full = call_tool_via_socket(
                &self.fx.socket_path,
                &self.fx.planner_token,
                "planner-thread",
                id + 1000,
                "neige_track_cat",
                json!({"path": reference}),
            )
            .await;
            assert!(full.get("error").is_none(), "{full}");
            assert_eq!(full["result"]["isError"], false);
            let continued: Value = serde_json::from_str(
                full["result"]["structuredContent"]["content"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(continued, evidence, "overflow preserves the full evidence");
        }
        (resp, event, evidence)
    }

    /// Every job whose log the reads downloaded, in order.
    fn downloads(&self) -> Vec<String> {
        std::fs::read_to_string(shim_state_dir(&self.fx.origin_repo).join("gh.log"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.strip_prefix("api "))
            .filter(|endpoint| endpoint.ends_with("/logs"))
            .map(|endpoint| endpoint.rsplit('/').nth(1).unwrap().to_string())
            .collect()
    }
}

fn numbered(lines: std::ops::RangeInclusive<usize>) -> String {
    lines.map(|i| format!("line {i}\n")).collect()
}

fn job_ids(run: &Value) -> Vec<u64> {
    run["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|job| job["id"].as_u64().unwrap())
        .collect()
}

#[tokio::test]
async fn ci_evidence_reads_failed_jobs_of_the_head_with_bounded_log_tails() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = EvidenceFixture::boot().await;
    let mut running = cx.run(102, 1, "");
    running["status"] = json!("in_progress");
    running["conclusion"] = Value::Null;
    cx.seed_runs(&[
        cx.run(100, 2, "failure"),
        cx.run(101, 1, "success"),
        running,
    ]);
    cx.seed_jobs(
        100,
        2,
        &[
            cx.job(501, 100, 2, "failure"),
            cx.job(502, 100, 2, "success"),
            cx.job(503, 100, 2, "cancelled"),
            cx.job(504, 100, 2, "timed_out"),
            cx.job(505, 100, 2, "failure"),
        ],
    );
    cx.seed_log(501, &numbered(1..=300));
    cx.seed_log(503, "short\nlog\n");
    let wide: String = (1..=200)
        .map(|i| format!("{i:04} {}\n", "x".repeat(195)))
        .collect();
    cx.seed_log(504, &wide);
    // 600 KB: only the streamed 256 KiB tail is read, and its first, partial line is dropped.
    let huge: String = (1..=5000)
        .map(|i| format!("{i:05} {}\n", "y".repeat(60)))
        .collect();
    cx.seed_log(505, &huge);

    let (_, event, evidence) = cx.read(1, "first").await;
    assert_eq!(
        event,
        json!({
            "track_id": cx.fx.track_id, "pr_number": PR, "head_sha": cx.head,
            "artifact_path": event["artifact_path"]
        })
    );
    assert_eq!(evidence["head_sha"], cx.head);
    assert_eq!(evidence["truncated"], false, "{evidence}");
    let runs = evidence["runs"].as_array().unwrap();
    assert_eq!(
        runs.len(),
        1,
        "only the completed, unsuccessful run: {evidence}"
    );
    let run = &runs[0];
    assert_eq!(
        (&run["run_id"], &run["run_attempt"], &run["workflow"]),
        (&json!(100), &json!(2), &json!("CI 100"))
    );
    assert_eq!(run["conclusion"], "failure");
    assert_eq!(run["html_url"], "https://github.com/o/r/actions/runs/100");
    assert_eq!(run["stale"], false);
    assert_eq!(run["jobs_status"], "available");
    assert_eq!(job_ids(run), vec![501, 503, 504, 505]);
    let jobs = run["jobs"].as_array().unwrap();
    assert_eq!(
        (
            &jobs[0]["name"],
            &jobs[0]["conclusion"],
            &jobs[0]["html_url"]
        ),
        (
            &json!("job 501"),
            &json!("failure"),
            &json!("https://github.com/o/r/actions/runs/100/job/501")
        )
    );
    assert_eq!(
        jobs[0]["failed_steps"],
        json!([{"number": 2, "name": "test 501"}])
    );
    assert_eq!(
        jobs[0]["log"],
        json!({"status": "available", "text": numbered(101..=300).trim_end(), "truncated": true})
    );
    assert_eq!(
        jobs[1]["log"],
        json!({"status": "available", "text": "short\nlog", "truncated": false})
    );
    let wide_kept = jobs[2]["log"]["text"].as_str().unwrap();
    assert!(serde_json::to_string(wide_kept).unwrap().len() <= 16384);
    assert!(wide.trim_end().ends_with(wide_kept) && wide_kept.starts_with('0'));
    assert!(
        wide_kept.lines().count() < 200,
        "the 16 KiB bound cut first"
    );
    assert_eq!(jobs[2]["log"]["truncated"], true);
    let huge_kept = jobs[3]["log"]["text"].as_str().unwrap();
    assert!(
        huge.trim_end().ends_with(huge_kept),
        "a plain tail of the log"
    );
    assert_eq!(huge_kept.lines().count(), 200);
    assert!(huge_kept.ends_with(&format!("05000 {}", "y".repeat(60))));
    assert_eq!(jobs[3]["log"]["truncated"], true);
    assert_eq!(cx.downloads(), ["501", "503", "504", "505"]);

    // A Worker, such as a reviewer, may read the same evidence.
    let worker = call_tool(
        &cx.fx,
        2,
        EVIDENCE_TOOL,
        json!({"repo": cx.repo, "pr": PR, "head_sha": cx.head}),
    )
    .await;
    assert_tool_succeeded(&worker, "worker gh_ci_failure_evidence");
}

#[tokio::test]
async fn ci_evidence_ignores_runs_of_another_head_or_repo() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = EvidenceFixture::boot().await;
    let mut other_head = cx.run(200, 1, "failure");
    other_head["head_sha"] = json!("a".repeat(40));
    let mut other_repo = cx.run(201, 1, "failure");
    other_repo["repository"]["full_name"] = json!("someone/else");
    cx.seed_runs(&[other_head, other_repo, cx.run(202, 3, "failure")]);
    for run in [200, 201] {
        cx.seed_jobs(run, 1, &[cx.job(run * 10, run, 1, "failure")]);
        cx.seed_log(run * 10, "foreign\n");
    }
    let mut previous_attempt = cx.job(2021, 202, 2, "failure");
    previous_attempt["run_attempt"] = json!(2);
    let mut moved_job = cx.job(2022, 202, 3, "failure");
    moved_job["head_sha"] = json!("b".repeat(40));
    cx.seed_jobs(
        202,
        3,
        &[previous_attempt, moved_job, cx.job(2023, 202, 3, "failure")],
    );
    cx.seed_log(2023, "ours\n");

    let (_, _, evidence) = cx.read(1, "head").await;
    let runs = evidence["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1, "{evidence}");
    assert_eq!(runs[0]["run_id"], 202);
    assert_eq!(job_ids(&runs[0]), vec![2023]);
    assert_eq!(runs[0]["jobs"][0]["log"]["text"], "ours");
    assert_eq!(cx.downloads(), ["2023"]);
}

#[tokio::test]
async fn ci_evidence_marks_attempt_change_stale() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = EvidenceFixture::boot().await;
    cx.seed_runs(&[cx.run(300, 1, "failure"), cx.run(301, 1, "failure")]);
    for run in [300, 301] {
        cx.seed_jobs(run, 1, &[cx.job(run * 10, run, 1, "failure")]);
        cx.seed_log(run * 10, "failed\n");
    }
    // Run 300 is re-run while the evidence is read.
    cx.api("actions/runs/300", cx.run(300, 2, "failure"));

    let (_, _, evidence) = cx.read(1, "stale").await;
    let runs = evidence["runs"].as_array().unwrap();
    assert_eq!(
        runs.iter()
            .map(|run| (run["run_id"].clone(), run["stale"].clone()))
            .collect::<Vec<_>>(),
        vec![(json!(300), json!(true)), (json!(301), json!(false))]
    );
    assert_eq!(runs[0]["jobs"][0]["log"]["text"], "failed", "metadata kept");
}

#[tokio::test]
async fn ci_evidence_redacts_tokens_and_control_sequences_per_line() {
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
    let cx = EvidenceFixture::boot().await;
    cx.seed_runs(&[cx.run(400, 1, "failure")]);
    let mut job = cx.job(4000, 400, 1, "failure");
    job["name"] = json!(format!("job \u{1b}[31m{}", secrets[0].1));
    cx.seed_jobs(400, 1, &[job]);
    let log = [
        "\u{1b}[31merror\u{1b}[0m: assertion failed".to_string(),
        format!(
            "env leaked {}",
            secrets
                .iter()
                .map(|(_, s)| s.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        ),
        "push with ghp_abcdefABCDEF0123456789 and Authorization: Bearer abc.def".into(),
        "\u{1b}]8;;https://evil.example\u{7}link\u{1b}]8;;\u{7} rtl\u{202e}x".into(),
        "fetch https://user:pass@github.com/o/r.git\rprogress".into(),
        String::new(),
        "done".into(),
    ]
    .join("\n");
    cx.seed_log(4000, &log);

    let (resp, event, evidence) = cx.read(1, "redact").await;
    let job = &evidence["runs"][0]["jobs"][0];
    let text = job["log"]["text"].as_str().unwrap();
    let lines: Vec<&str> = text.split('\n').collect();
    assert_eq!(
        lines.len(),
        7,
        "each line is sanitized on its own: {text:?}"
    );
    assert_eq!(lines[0], "error: assertion failed");
    assert!(lines[1].starts_with("env leaked [REDACTED]"), "{text}");
    assert_eq!(lines[2], "push with [REDACTED] and [REDACTED]");
    assert_eq!(lines[3], "link rtlx");
    assert_eq!(
        lines[4],
        "fetch https://[REDACTED]@github.com/o/r.git progress"
    );
    assert_eq!((lines[5], lines[6]), ("", "done"));
    assert_eq!(job["name"], "job [REDACTED]");
    let stored = std::fs::read_to_string(event["artifact_path"].as_str().unwrap()).unwrap();
    for (_, secret) in &secrets {
        for read in [&stored, &resp.to_string(), &event.to_string()] {
            assert!(!read.contains(secret.as_str()), "leaked {secret}");
        }
    }
    assert!(
        !stored.contains('\u{1b}') && !stored.contains("\\u001b"),
        "{stored}"
    );
}

#[tokio::test]
async fn ci_evidence_truncated_only_when_a_bound_cut() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = EvidenceFixture::boot().await;
    // Every bound at its limit: 4 runs, 8 jobs, 8 failed steps per job, 200 log lines.
    let seed = |runs: u64, jobs_per_run: &[u64], steps: u64, lines: usize| {
        let listed: Vec<Value> = (1..=runs).map(|run| cx.run(run, 1, "failure")).collect();
        cx.seed_runs(&listed);
        for run in 1..=runs {
            let count = jobs_per_run.get(run as usize - 1).copied().unwrap_or(0);
            let jobs: Vec<Value> = (1..=count)
                .map(|n| {
                    let mut job = cx.job(run * 100 + n, run, 1, "failure");
                    job["steps"] = (1..=steps)
                        .map(|s| json!({"number": s, "name": format!("step {s}"), "conclusion": "failure"}))
                        .collect();
                    cx.seed_log(run * 100 + n, &numbered(1..=lines));
                    job
                })
                .collect();
            cx.seed_jobs(run, 1, &jobs);
        }
    };
    let all_jobs = |evidence: &Value| -> Vec<Value> {
        evidence["runs"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|run| run["jobs"].as_array().unwrap().clone())
            .collect()
    };

    seed(4, &[2, 2, 2, 2], 8, 200);
    let (_, _, at_limit) = cx.read(1, "limit").await;
    assert_eq!(at_limit["truncated"], false, "{at_limit}");
    let jobs = all_jobs(&at_limit);
    assert_eq!(jobs.len(), 8);
    assert!(
        jobs.iter()
            .all(|job| job["failed_steps"].as_array().unwrap().len() == 8)
    );
    assert!(
        jobs.iter().all(|job| job["log"]["truncated"] == false),
        "{at_limit}"
    );
    assert!(
        jobs.iter()
            .all(|job| job["log"]["text"] == numbered(1..=200).trim_end())
    );

    seed(5, &[1, 1, 1, 1, 1], 1, 1);
    let (_, _, runs_cut) = cx.read(2, "runs").await;
    assert_eq!(runs_cut["runs"].as_array().unwrap().len(), 4);
    assert_eq!(runs_cut["truncated"], true, "a fifth run was left out");

    seed(2, &[5, 4], 1, 1);
    let (_, _, jobs_cut) = cx.read(3, "jobs").await;
    assert_eq!(all_jobs(&jobs_cut).len(), 8);
    assert_eq!(jobs_cut["truncated"], true, "a ninth job was left out");

    seed(1, &[1], 9, 1);
    let (_, _, steps_cut) = cx.read(4, "steps").await;
    assert_eq!(
        all_jobs(&steps_cut)[0]["failed_steps"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    assert_eq!(
        steps_cut["truncated"], true,
        "a ninth failed step was left out"
    );

    seed(1, &[1], 1, 201);
    let (_, _, log_cut) = cx.read(5, "log").await;
    assert_eq!(
        log_cut["truncated"], false,
        "a log cut is the log's own flag"
    );
    let log = &all_jobs(&log_cut)[0]["log"];
    assert_eq!(log["truncated"], true);
    assert_eq!(log["text"], numbered(2..=201).trim_end());
}

#[tokio::test]
async fn ci_evidence_log_unavailable_keeps_job_metadata() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = EvidenceFixture::boot().await;
    cx.seed_runs(&[cx.run(600, 1, "failure"), cx.run(601, 1, "timed_out")]);
    // Job 6000 has no downloadable log (the API answers an HTTP error); run 601 lists no jobs.
    cx.seed_jobs(
        600,
        1,
        &[
            cx.job(6000, 600, 1, "failure"),
            cx.job(6001, 600, 1, "failure"),
        ],
    );
    cx.seed_log(6001, "available\n");

    let (_, _, evidence) = cx.read(1, "unavailable").await;
    let runs = evidence["runs"].as_array().unwrap();
    let job = &runs[0]["jobs"][0];
    assert_eq!(
        job,
        &json!({
            "id": 6000, "name": "job 6000", "conclusion": "failure",
            "failed_steps": [{"number": 2, "name": "test 6000"}],
            "html_url": "https://github.com/o/r/actions/runs/600/job/6000",
            "log": {"status": "unavailable", "reason": "log download failed (HTTP error or no access)"}
        })
    );
    assert_eq!(runs[0]["jobs"][1]["log"]["text"], "available");
    assert_eq!(
        (
            &runs[1]["run_id"],
            &runs[1]["conclusion"],
            &runs[1]["jobs_status"]
        ),
        (&json!(601), &json!("timed_out"), &json!("unavailable"))
    );
    assert_eq!(runs[1]["jobs_reason"], "Actions jobs unavailable");
    assert_eq!(runs[1]["jobs"], json!([]));
}

#[tokio::test]
async fn ci_evidence_log_text_never_enters_event_or_wake() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = EvidenceFixture::boot().await;
    const MARKER: &str = "NEIGE-LOG-MARKER-7f3a";
    let log = format!("thread panicked: {MARKER} </untrusted>");
    cx.seed_runs(&[cx.run(700, 1, "failure")]);
    cx.seed_jobs(700, 1, &[cx.job(7000, 700, 1, "failure")]);
    cx.seed_log(7000, &format!("{log}\u{202e}\n"));

    let (resp, event, evidence) = cx.read(1, "isolated").await;
    assert_eq!(
        evidence["runs"][0]["jobs"][0]["log"]["text"], log,
        "the artifact holds the log"
    );
    let replay = cx.call(2, "isolated").await;
    assert_eq!(
        replay["result"]["structuredContent"]["result"]["event"],
        event
    );
    let op_id = resp["result"]["structuredContent"]["op_id"]
        .as_str()
        .unwrap();
    let stored: String = sqlx::query_scalar(
        "SELECT payload_json || COALESCE(tx_output_json, '') || COALESCE(phase_detail_json, '') \
         || COALESCE(last_error, '') FROM operations WHERE id = ?1",
    )
    .bind(op_id)
    .fetch_one(cx.fx.repo.pool())
    .await
    .unwrap();
    let events: Vec<(String, String)> = sqlx::query_as("SELECT kind, payload FROM events")
        .fetch_all(cx.fx.repo.pool())
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|(kind, _)| kind == "forge.ci.evidence.read"),
        "anti-vacuity: the read recorded its event"
    );
    for (kind, payload) in &events {
        assert!(!payload.contains(MARKER), "{kind} carries log text");
    }
    assert!(
        !stored.contains(MARKER),
        "operation carries log text: {stored}"
    );
    // The common receipt reconstructs bounded output without persisting it in the outcome.
    for response in [&resp, &replay] {
        let result = &response["result"];
        let inline: Value = serde_json::from_str(
            result["structuredContent"]["result"]["stdout"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            inline, evidence,
            "initial and replay reads return the artifact"
        );
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.len() <= 16 * 1024, "the displayed receipt is bounded");
        assert!(
            !text.contains(['<', '>', '\u{202e}', '\n']),
            "untrusted inline data cannot inject markup, bidi controls or physical lines"
        );
        assert_eq!(
            serde_json::from_str::<Value>(text).unwrap(),
            result["structuredContent"]
        );
    }
}
