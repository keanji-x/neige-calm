//! #2617 item E: `plugin_gitforge_gh_run_rerun` posts a rerun only after the user clicked
//! Approve on the ask bound to that exact request, through the kernel's approval gate on the one
//! forge submit entry. Driven end to end: the Planner's tool call, the answer route, the gh shim.
use super::*;
use crate::support::gh_shim::run_gh;
use crate::support::git_helpers::git_stdout_no_cwd;

const RERUN_TOOL: &str = "plugin_gitforge_gh_run_rerun";
const RUN: u64 = 100;

struct RerunFixture {
    fx: Fixture,
    repo_arg: String,
    branch: String,
    pr: u64,
    _env: ForgeTestEnv,
}

impl RerunFixture {
    async fn boot(branch: &str) -> Self {
        let env = setup_forge_env();
        let fx = boot_fixture().await;
        let repo_arg = fx.origin_repo.display().to_string();
        let branch = format!("rerun-{branch}");
        git_stdout_no_cwd(["--git-dir", &repo_arg, "branch", &branch, "main"]);
        let created = run_gh(
            &env._path_dir.path().join("gh"),
            &[
                "pr", "create", "--repo", &repo_arg, "--head", &branch, "--base", "main",
            ],
        );
        assert!(created.status.success(), "{created:?}");
        let created: Value = serde_json::from_slice(&created.stdout).expect("pr create json");
        let cx = Self {
            pr: created["number"].as_u64().expect("pr number"),
            fx,
            repo_arg,
            branch,
            _env: env,
        };
        cx.seed_run(&cx.head_sha(), 1, "completed");
        cx
    }

    fn head_sha(&self) -> String {
        git_stdout_no_cwd(["--git-dir", &self.repo_arg, "rev-parse", &self.branch])
    }

    /// What `GET actions/runs/{RUN}` reports.
    fn seed_run(&self, head_sha: &str, run_attempt: u64, status: &str) {
        let dir = shim_state_dir(&self.fx.origin_repo).join("actions");
        std::fs::create_dir_all(&dir).unwrap();
        let endpoint = format!("repos/{}/actions/runs/{RUN}", self.repo_arg);
        let run = json!({
            "id": RUN, "head_sha": head_sha, "run_attempt": run_attempt, "status": status,
            "repository": { "full_name": self.repo_arg }
        });
        std::fs::write(dir.join(endpoint.replace('/', "_")), run.to_string()).unwrap();
    }

    /// A new commit on the PR branch, as a push would land it.
    fn move_head(&self) {
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
    }

    /// The rerun of [`RUN`]'s attempt 1 at the current head, under `idem`.
    fn args(&self, idem: &str) -> Value {
        json!({
            "repo": self.repo_arg, "pr": self.pr, "head_sha": self.head_sha(), "run_id": RUN,
            "run_attempt": 1, "reason": "shard 3 timed out fetching a crate", "idem": idem
        })
    }

    /// The Planner's call through the tool registry, shaped as an MCP response. After an answer
    /// the fixture's Planner socket token no longer resolves to a session, so a call after an
    /// answer runs in process, as the merge ask tests do.
    async fn call(&self, _id: i64, args: &Value) -> Value {
        match call_review_tool(&self.fx, RERUN_TOOL, args.clone()).await {
            Ok(content) => json!({ "result": { "structuredContent": content } }),
            Err(error) => json!({ "error": error }),
        }
    }

    /// The Planner's call over its MCP socket, as the full response.
    async fn call_mcp(&self, id: i64, args: &Value) -> Value {
        call_tool_via_socket(
            &self.fx.socket_path,
            &self.fx.planner_token,
            "planner-thread",
            id,
            RERUN_TOOL,
            args.clone(),
        )
        .await
    }

    /// The call when it is gated, its ask clicked Approve, and the repeated call's response.
    async fn call_approved(&self, id: i64, args: &Value) -> Value {
        let first = self.call(id, args).await;
        let Some(ask_id) = awaiting(&first) else {
            return first;
        };
        assert_eq!(
            self.answer(ask_id, json!([{ "option": 0 }])).await,
            StatusCode::NO_CONTENT
        );
        self.call(id + 1, args).await
    }

    async fn answer(&self, ask_id: i64, answers: Value) -> StatusCode {
        app_router_for_fixture(&self.fx)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/tracks/{}/asks/{ask_id}/answer",
                        self.fx.track_id
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "answers": answers }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    /// The rerun requests GitHub received.
    fn posts(&self) -> u64 {
        shim_counter(&shim_state_dir(&self.fx.origin_repo).join("rerun_failed_count"))
    }

    async fn asks(&self) -> Vec<EventRow> {
        event_rows(&self.fx.repo, "ask.requested").await
    }
}

/// The ask the call awaits, when it awaits one.
fn awaiting(response: &Value) -> Option<i64> {
    let content = &response["result"]["structuredContent"];
    (content["status"] == "awaiting_approval").then(|| content["ask_id"].as_i64().unwrap())
}

/// The refusal message of a JSON-RPC error response.
fn refusal(response: &Value) -> &str {
    response["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a refusal: {response:#}"))
}

fn content(response: &Value) -> &Value {
    assert!(response.get("error").is_none(), "{response:#}");
    &response["result"]["structuredContent"]
}

#[tokio::test]
async fn rerun_without_recorded_approval_posts_nothing_and_asks_once() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = RerunFixture::boot("ask").await;
    let args = cx.args("r-1");
    let first = cx.call_mcp(1, &args).await;
    let ask_id = awaiting(&first).unwrap_or_else(|| panic!("awaits approval: {first:#}"));
    assert_eq!(content(&first)["ok"], false);
    let asks = cx.asks().await;
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0].id, ask_id);
    let ask = &asks[0].payload;
    assert_eq!(ask["action"]["kind"], "forge_action");
    assert_eq!(ask["delivery"], "wake");
    assert_eq!(
        ask["questions"][0]["options"],
        json!(["Approve", "Decline"])
    );
    let title = ask["questions"][0]["title"].as_str().unwrap();
    for part in [
        format!("run {RUN} attempt 1"),
        format!("PR #{}", cx.pr),
        cx.head_sha(),
        "shard 3 timed out".to_string(),
    ] {
        assert!(title.contains(&part), "{title} names {part}");
    }

    // A repeated call while the ask is open gets the same ask back.
    let again = cx.call_mcp(2, &args).await;
    assert_eq!(awaiting(&again), Some(ask_id), "{again:#}");
    assert_eq!(cx.asks().await.len(), 1);
    assert_eq!(cx.posts(), 0, "nothing is posted without an Approve click");
}

#[tokio::test]
async fn rerun_after_approve_click_posts_once_and_replays() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = RerunFixture::boot("approve").await;
    let args = cx.args("r-1");
    let posted = cx.call_approved(1, &args).await;
    let result = content(&posted);
    assert_eq!(result["ok"], true, "{posted:#}");
    assert_eq!(
        result["after_rerun"],
        json!({ "run_id": RUN, "run_attempt": 1 })
    );
    assert!(
        result["next"]
            .as_str()
            .unwrap()
            .contains("plugin_gitforge_gh_pr_checks")
    );
    assert_eq!(cx.posts(), 1);

    let replayed = cx.call(3, &args).await;
    assert_eq!(content(&replayed)["op_id"], result["op_id"]);
    assert_eq!(cx.posts(), 1, "a replay posts nothing");
}

#[tokio::test]
async fn rerun_decline_or_typed_answer_never_posts() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = RerunFixture::boot("decline").await;
    for (idem, answers) in [
        ("r-decline", json!([{ "option": 1 }])),
        ("r-typed", json!([{ "text": "Approve" }])),
    ] {
        let args = cx.args(idem);
        let first = cx.call(1, &args).await;
        let ask_id = awaiting(&first).unwrap_or_else(|| panic!("awaits approval: {first:#}"));
        assert_eq!(cx.answer(ask_id, answers).await, StatusCode::NO_CONTENT);
        let asked = cx.asks().await.len();
        for id in [2, 3] {
            let declined = cx.call(id, &args).await;
            assert!(
                refusal(&declined).contains("rerun-declined"),
                "{idem}: {declined:#}"
            );
        }
        assert_eq!(cx.asks().await.len(), asked, "{idem}: no new ask");
    }
    assert_eq!(cx.posts(), 0);
}

#[tokio::test]
async fn rerun_approval_is_bound_to_run_attempt_and_idem() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = RerunFixture::boot("bound").await;
    let approved = cx.args("r-1");
    let first = cx.call(1, &approved).await;
    let ask_id = awaiting(&first).unwrap_or_else(|| panic!("awaits approval: {first:#}"));
    assert_eq!(
        cx.answer(ask_id, json!([{ "option": 0 }])).await,
        StatusCode::NO_CONTENT
    );
    let mut other_attempt = approved.clone();
    other_attempt["run_attempt"] = json!(2);
    let mut other_reason = approved.clone();
    other_reason["reason"] = json!("another reason");
    for (label, args) in [
        ("another attempt", other_attempt),
        ("another idem", cx.args("r-2")),
        ("another reason under the same idem", other_reason),
    ] {
        let response = cx.call(2, &args).await;
        let other = awaiting(&response).unwrap_or_else(|| panic!("{label}: {response:#}"));
        assert_ne!(other, ask_id, "{label}: a new ask");
    }
    assert_eq!(cx.asks().await.len(), 4);
    assert_eq!(cx.posts(), 0, "the approval covers only its own request");

    let posted = cx.call(3, &approved).await;
    assert_eq!(content(&posted)["ok"], true, "{posted:#}");
    assert_eq!(cx.posts(), 1);
}

#[tokio::test]
async fn rerun_refuses_advanced_attempt_or_moved_head_without_post() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = RerunFixture::boot("refuse").await;
    // GitHub already reran the run: it reports a later attempt.
    cx.seed_run(&cx.head_sha(), 2, "completed");
    let advanced = cx.call_approved(1, &cx.args("r-advanced")).await;
    assert!(
        refusal(&advanced).contains("rerun-failed")
            && refusal(&advanced).contains("exited with code 21"),
        "{advanced:#}"
    );

    cx.seed_run(&cx.head_sha(), 1, "completed");
    let stale = cx.args("r-moved");
    cx.move_head();
    let moved = cx.call_approved(3, &stale).await;
    assert!(
        refusal(&moved).contains("rerun-failed") && refusal(&moved).contains("exited with code 20"),
        "{moved:#}"
    );
    assert_eq!(cx.posts(), 0);
}

#[tokio::test]
async fn rerun_is_not_listed_or_callable_for_workers() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = RerunFixture::boot("worker").await;
    let listed = |token: String, thread: String| {
        let socket = cx.fx.socket_path.clone();
        async move {
            let (mut rd, mut wr) = connect(&socket).await;
            handshake(&mut rd, &mut wr, &token).await;
            send_frame(&mut wr, tools_list_frame(2, &thread)).await;
            let list = recv_frame(&mut rd).await;
            list["result"]["tools"]
                .as_array()
                .expect("tools array")
                .iter()
                .any(|tool| tool["name"] == RERUN_TOOL)
        }
    };
    assert!(listed(cx.fx.planner_token.clone(), "planner-thread".into()).await);
    assert!(!listed(cx.fx.raw_token.clone(), cx.fx.thread_id.clone()).await);

    let called = call_tool(&cx.fx, 3, RERUN_TOOL, cx.args("r-worker")).await;
    assert!(called.get("error").is_some(), "{called:#}");
    assert!(cx.asks().await.is_empty(), "a worker raises no ask");
    assert_eq!(cx.posts(), 0);
}

#[tokio::test]
async fn rerun_lost_result_is_unknown_and_never_reposted() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cx = RerunFixture::boot("lost").await;
    // The POST lands, then the Engine stops the action before any result survives.
    let state = shim_state_dir(&cx.fx.origin_repo);
    let block = ShimBlock::new(&state, "rerun_lost");
    let args = cx.args("r-lost");
    let waiting = Arc::new(tokio::sync::Notify::new());
    cx.fx
        ._runtime
        .install_wait_entered_hook_for_test(waiting.clone());
    let call = cx.call_approved(1, &args);
    tokio::pin!(call);
    // Suspend only once submission has returned and wait holds no transaction or claim.
    // Suspending at the shim counter can freeze the caller inside a database await.
    tokio::select! {
        _=waiting.notified()=>{},
        result=&mut call=>panic!("rerun returned before the lost-response seam: {result:?}"),
    }
    wait_for_counter(&state.join("rerun_failed_count"), 1).await;
    let op: String = sqlx::query_scalar(
        "SELECT id FROM operations WHERE kind='forge-action' AND phase='parked'",
    )
    .fetch_one(cx.fx.repo.pool())
    .await
    .unwrap();
    settle_parked_execution_without_result(&cx.fx, &op).await;
    let recovery = boot_recovery_runtime(&cx.fx).await;
    let plan = recovery.recover_on_boot().await.unwrap();
    recovery.apply_recovery(plan).await.unwrap();
    let lost = timeout(Duration::from_secs(5), call)
        .await
        .expect("recovery wakes the original caller");
    assert!(refusal(&lost).contains("rerun-unknown"), "{lost:#}");
    assert_eq!(cx.posts(), 1);
    block.release();
    let again = cx.call(3, &args).await;
    assert!(refusal(&again).contains("rerun-unknown"), "{again:#}");
    assert_eq!(cx.posts(), 1, "an unknown rerun is never posted again");
}
