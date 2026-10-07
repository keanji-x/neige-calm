//! Production boot recovery, with an external API shim and missing durable receipts.
use super::*;
const CREATE: &str = "plugin_gitforge_gh_issue_create";
const URL: &str = "https://github.com/owner/repo/issues/731";
struct Api {
    shim: TempDir,
    _results: TempDir,
    _path: EnvGuard,
    _result_env: EnvGuard,
}
impl Api {
    fn new() -> Self {
        let shim = short_tempdir("create-recovery-api").unwrap();
        support::issue_api::write_shim(shim.path());
        let results = short_tempdir("create-recovery-results").unwrap();
        let path = EnvGuard::set("PATH", prepend_to_path(shim.path()));
        let result_env = EnvGuard::set("NEIGE_FORGE_RESULTS_DIR", results.path());
        Self {
            shim,
            _results: results,
            _path: path,
            _result_env: result_env,
        }
    }
    fn state(&self) -> PathBuf {
        self.shim.path().join("state")
    }
    fn write(&self, file: &str, value: &Value) {
        std::fs::write(self.state().join(file), value.to_string()).unwrap();
    }
    fn closed_second_page(&self) -> Value {
        let mut row: Value = serde_json::from_str(
            &std::fs::read_to_string(self.state().join("created.json")).unwrap(),
        )
        .unwrap();
        row["state"] = json!("closed");
        let mut pr = row.clone();
        pr["pull_request"] = json!({"url":"pull"});
        self.write("page1.json", &json!(vec![pr; 100]));
        self.write("page2.json", &json!([row]));
        row
    }
}
fn args() -> Value {
    json!({"repo":"owner/repo","title":"Recovery title","body":"Recovery body","idem":"recover-one"})
}
async fn recover(fx: &Fixture, op: &str, lease: &str) -> OperationResult {
    mark_parked_artifacts_dead(&fx.repo, op).await;
    mark_workspace_lease_stale_for_boot(&fx.repo, lease).await;
    let runtime = boot_recovery_runtime(fx).await;
    let plan = runtime.recover_on_boot().await.unwrap();
    runtime.apply_recovery(plan).await.unwrap();
    let result = wait_for_recovery_result(&runtime, op).await;
    // Boot reclaims the stale lease. Give the authenticated caller a fresh lease
    // before asking the MCP entry point to replay the durable outcome.
    let (card, track, path): (String, String, String) =
        sqlx::query_as("SELECT card_id,track_id,path FROM workspace_leases WHERE lease_id=?1")
            .bind(lease)
            .fetch_one(fx.repo.pool())
            .await
            .unwrap();
    let mut tx = fx.repo.pool().begin().await.unwrap();
    insert_workspace_lease(&mut tx, &card, &track, &path).await;
    tx.commit().await.unwrap();
    result
}
fn assert_real_result(outcome: &OperationOutcome) {
    match outcome {
        OperationOutcome::Succeeded { result } => {
            assert_eq!(result["event_kind"], "forge.issue.created");
            assert_eq!(result["event"]["issue_number"], 731);
            assert_eq!(result["event"]["issue_url"], URL);
        }
        other => panic!("expected success: {other:?}"),
    }
}

#[tokio::test]
async fn git_forge_issue_create_recovers_number_and_url_without_recreating() {
    let _lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let api = Api::new();
    let fx = boot_fixture().await;
    let block = ShimBlock::new(&api.state(), "issue_create");
    let response = call_tool(&fx, 30, CREATE, args()).await;
    assert_tool_succeeded(&response, CREATE);
    let op = op_id_from_response(&response);
    wait_for_counter(&api.state().join("issue_create_count"), 1).await;
    wait_for_operation_phase(&fx.repo, &op, "parked").await;
    let result_path = operation_result_path(&fx.repo, &op).await;
    assert_result_files_absent(&result_path);
    api.closed_second_page();
    let result = recover(&fx, &op, &fx.lease_id).await;
    assert_real_result(&result.outcome);
    assert_eq!(shim_counter(&api.state().join("issue_create_count")), 1);
    let replay = call_tool(&fx, 31, CREATE, args()).await;
    assert_eq!(replay["result"]["structuredContent"]["op_id"], op);
    assert_eq!(
        replay["result"]["structuredContent"]["result"]["event"]["issue_url"],
        URL
    );
    block.release();
    wait_for_result_code_file(&result_path).await;
    assert_eq!(shim_counter(&api.state().join("issue_create_count")), 1);
    let rows = event_rows(&fx.repo, "forge.issue.created").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].payload["issue_number"], 731);
    assert_eq!(rows[0].payload["issue_url"], URL);
    // A separate live request obtains the same API number/URL contract.
    std::fs::remove_file(api.state().join("block_issue_create")).unwrap();
    let mut live_args = args();
    live_args["idem"] = json!("live");
    let live = call_tool(&fx, 32, CREATE, live_args.clone()).await;
    wait_for_operation_phase(&fx.repo, &op_id_from_response(&live), "succeeded").await;
    let done = call_tool(&fx, 33, CREATE, live_args).await;
    assert_eq!(
        done["result"]["structuredContent"]["result"]["event"]["issue_url"],
        URL
    );
    assert_eq!(
        done["result"]["structuredContent"]["result"]["event"]["issue_number"],
        731
    );
    fx.plugin_host.stop(PLUGIN_ID).await.unwrap();
}

#[tokio::test]
async fn git_forge_issue_create_probe_failure_is_unknown() {
    let _lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    for fault in [
        "documents",
        "outage",
        "invalid",
        "missing",
        "missing-body",
        "missing-body-with-match",
        "page2.fail",
        "multiple",
        "output.fail",
    ] {
        let api = Api::new();
        let fx = boot_fixture().await;
        let block = ShimBlock::new(&api.state(), "issue_create");
        let response = call_tool(&fx, 30, CREATE, args()).await;
        assert_tool_succeeded(&response, CREATE);
        let op = op_id_from_response(&response);
        wait_for_counter(&api.state().join("issue_create_count"), 1).await;
        wait_for_operation_phase(&fx.repo, &op, "parked").await;
        let result_path = operation_result_path(&fx.repo, &op).await;
        assert_result_files_absent(&result_path);
        let mut row = api.closed_second_page();
        match fault {
            "missing-body" | "missing-body-with-match" => {
                let complete = row.clone();
                row.as_object_mut().unwrap().remove("body");
                let inventory = if fault == "missing-body" {
                    json!([row])
                } else {
                    json!([row, complete])
                };
                api.write("page2.json", &inventory);
            }
            "missing" => {
                row.as_object_mut().unwrap().remove("html_url");
                api.write("page2.json", &json!([row]));
            }
            "multiple" => {
                let mut other = row.clone();
                other["number"] = json!(732);
                other["html_url"] = json!("https://github.com/owner/repo/issues/732");
                api.write("page2.json", &json!([row, other]));
            }
            name => std::fs::write(api.state().join(name), "").unwrap(),
        }
        let result = recover(&fx, &op, &fx.lease_id).await;
        assert!(
            matches!(result.outcome, OperationOutcome::Failed { ref last_error_class, .. } if last_error_class.as_deref() == Some("gate-infra")),
            "{fault}: {:?}",
            result.outcome
        );
        assert!(
            event_rows(&fx.repo, "forge.issue.created").await.is_empty(),
            "{fault}"
        );
        let replay = call_tool(&fx, 31, CREATE, args()).await;
        assert_eq!(replay["result"]["isError"], true, "{fault}: {replay}");
        assert_eq!(shim_counter(&api.state().join("issue_create_count")), 1);
        block.release();
        wait_for_result_code_file(&result_path).await;
        assert!(event_rows(&fx.repo, "forge.issue.created").await.is_empty());
        assert_eq!(
            shim_counter(&api.state().join("issue_create_count")),
            1,
            "{fault}"
        );
        fx.plugin_host.stop(PLUGIN_ID).await.unwrap();
    }
}

#[tokio::test]
async fn git_forge_issue_create_recovery_does_not_borrow_another_caller() {
    let _lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let api = Api::new();
    let fx = boot_fixture().await;
    let first = call_tool(&fx, 30, CREATE, args()).await;
    assert_tool_succeeded(&first, CREATE);
    wait_for_operation_phase(&fx.repo, &op_id_from_response(&first), "succeeded").await;
    let row: Value =
        serde_json::from_str(&std::fs::read_to_string(api.state().join("created.json")).unwrap())
            .unwrap();
    api.write("page1.json", &json!([row]));
    // Same track and request, different authenticated card; B never writes.
    sqlx::query("UPDATE workspace_leases SET state='released' WHERE lease_id=?1")
        .bind(&fx.lease_id)
        .execute(fx.repo.pool())
        .await
        .unwrap();
    let caller = create_worker_caller(
        &fx.repo,
        &fx.card_role_cache,
        TrackId::from(fx.track_id.clone()),
        &fx.track_cwd,
    )
    .await;
    assert_eq!(caller.track_id, fx.track_id);
    assert_ne!(caller.card_id, fx.worker_card_id);
    let block = ShimBlock::new(&api.state(), "issue_create_before");
    let second = call_tool_via_socket(
        &fx.socket_path,
        &caller.raw_token,
        &caller.thread_id,
        31,
        CREATE,
        args(),
    )
    .await;
    assert_tool_succeeded(&second, CREATE);
    let op = op_id_from_response(&second);
    wait_for_operation_phase(&fx.repo, &op, "parked").await;
    let process = parked_process_group_guard(&fx.repo, &op).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !api.state().join("issue_create_before_started").exists() {
        assert!(Instant::now() < deadline);
        sleep(Duration::from_millis(20)).await;
    }
    let result_path = operation_result_path(&fx.repo, &op).await;
    assert_result_files_absent(&result_path);
    let result = recover(&fx, &op, &caller.lease_id).await;
    assert!(
        matches!(result.outcome, OperationOutcome::Failed { ref last_error_class, .. } if last_error_class.as_deref() == Some("action-not-landed")),
        "cannot borrow A: {:?}",
        result.outcome
    );
    assert_eq!(shim_counter(&api.state().join("issue_create_count")), 1);
    assert_eq!(event_rows(&fx.repo, "forge.issue.created").await.len(), 1);
    drop(process);
    block.release();
    fx.plugin_host.stop(PLUGIN_ID).await.unwrap();
}
