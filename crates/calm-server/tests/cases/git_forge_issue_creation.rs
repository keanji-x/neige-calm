//! Issue contracts exercised through registered MCP tools.
use super::*;
const CREATE: &str = "plugin_gitforge_gh_issue_create";
const SEARCH: &str = "plugin_gitforge_gh_issue_search";
const VIEW: &str = "plugin_gitforge_gh_issue_view";

struct Api {
    shim: TempDir,
    _results: TempDir,
    _path: EnvGuard,
    _result_env: EnvGuard,
}
impl Api {
    fn new() -> Self {
        let shim = short_tempdir("issue-api").unwrap();
        support::issue_api::write_shim(shim.path());
        let results = short_tempdir("issue-results").unwrap();
        let path = EnvGuard::set(
            "PATH",
            format!(
                "{}:{}",
                shim.path().display(),
                std::env::var("PATH").unwrap()
            ),
        );
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
    fn write(&self, file: &str, data: &Value) {
        std::fs::write(self.state().join(file), data.to_string()).unwrap();
    }
}
fn issue(number: u64, state: &str, title: &str, body: &str) -> Value {
    json!({"number":number,"url":format!("https://github.com/owner/repo/issues/{number}"),"state":state,"title":title,"body":body,"labels":[{"name":"bug"}]})
}
fn stdout(response: &Value) -> Value {
    assert_eq!(response["result"]["isError"], false, "{response}");
    serde_json::from_str(
        response["result"]["structuredContent"]["result"]["stdout"]
            .as_str()
            .unwrap(),
    )
    .unwrap()
}
async fn wait(fx: &Fixture, response: &Value) -> String {
    assert_eq!(response["result"]["isError"], false, "{response}");
    let op = response["result"]["structuredContent"]["op_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let phase: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id=?1")
            .bind(&op)
            .fetch_one(fx.repo.pool())
            .await
            .unwrap();
        if phase == "succeeded" {
            return op;
        }
        assert!(
            !["failed", "cancelled"].contains(&phase.as_str()),
            "{phase}"
        );
        assert!(Instant::now() < deadline, "stuck {phase}");
        sleep(Duration::from_millis(20)).await;
    }
}
async fn call(fx: &Fixture, caller: &(String, String), id: i64, tool: &str, args: Value) -> Value {
    call_tool_as(fx, &caller.0, &caller.1, id, tool, args).await
}

#[tokio::test]
async fn planner_issue_create_is_registered_and_deduplicated() {
    let _lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let api = Api::new();
    let fx = boot_fixture().await;
    let caller = git_forge_track_worktree::issue_planner(&fx).await;
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &caller.0).await;
    send_frame(&mut wr, tools_list_frame(2, &caller.1)).await;
    let list = recv_frame(&mut rd).await;
    for name in [CREATE, SEARCH] {
        let descriptor = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .expect("registered issue tool");
        assert_eq!(descriptor["annotations"]["readOnlyHint"], name == SEARCH);
        assert_eq!(descriptor["inputSchema"]["additionalProperties"], false);
        send_frame(&mut wr, json!({"jsonrpc":"2.0","id":20,"method":"neige/cli","params":{"argv":["tool","describe","--name",name,"--json"]}})).await;
        let described = recv_frame(&mut rd).await;
        assert_eq!(described["result"]["exit"], 0, "{described}");
        let catalog: Value =
            serde_json::from_str(described["result"]["stdout"].as_str().unwrap()).unwrap();
        assert_eq!(catalog["plugin"], PLUGIN_ID);
        assert_eq!(catalog["kind"], "forge-action");
    }
    let args = json!({"repo":"Owner/Repo","title":"Title \" $(touch forbidden)","body":"Body\nquotes ' \" $(touch forbidden)","idem":"request:1"});
    let first = call(&fx, &caller, 3, CREATE, args.clone()).await;
    let op = wait(&fx, &first).await;
    let again = call(&fx, &caller, 4, CREATE, args.clone()).await;
    assert_eq!(again["result"]["structuredContent"]["op_id"], op);
    let result = &again["result"]["structuredContent"]["result"];
    assert_eq!(result["event_kind"], "forge.issue.created");
    assert_eq!(result["event"]["issue_number"], 731);
    assert_eq!(
        result["event"]["issue_url"],
        "https://github.com/owner/repo/issues/731"
    );
    assert_eq!(
        std::fs::read_to_string(api.state().join("issue_create_count")).unwrap(),
        "1"
    );
    let stored: Value =
        serde_json::from_str(&std::fs::read_to_string(api.state().join("created.json")).unwrap())
            .unwrap();
    assert_eq!(stored["title"], args["title"]);
    assert!(
        stored["body"]
            .as_str()
            .unwrap()
            .starts_with(args["body"].as_str().unwrap())
    );
    assert!(!fx.lease_abs.join("forbidden").exists());
    assert_eq!(event_rows(&fx.repo, "forge.issue.created").await.len(), 1);
    let mut alias = args.clone();
    alias["repo"] = json!("github.com/owner/repo");
    let same = call(&fx, &caller, 5, CREATE, alias).await;
    assert_eq!(same["result"]["structuredContent"]["op_id"], op);
    let mut spoof = args;
    spoof["caller"] = json!({"card_id":"other"});
    let rejected = call(&fx, &caller, 6, CREATE, spoof).await;
    assert_eq!(rejected["error"]["code"], -32602, "{rejected}");
}

#[tokio::test]
async fn planner_issue_create_rejects_changed_content_same_idem() {
    let _lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let api = Api::new();
    let fx = boot_fixture().await;
    let caller = git_forge_track_worktree::issue_planner(&fx).await;
    let args = json!({"repo":"owner/repo","title":"Title","body":"Body","idem":"one"});
    wait(&fx, &call(&fx, &caller, 3, CREATE, args.clone()).await).await;
    for field in ["title", "body"] {
        let mut changed = args.clone();
        changed[field] = json!("Changed");
        let conflict = call(&fx, &caller, 4, CREATE, changed).await;
        assert_eq!(conflict["result"]["isError"], true, "{conflict}");
        assert!(
            conflict.to_string().contains("idempotency key reused"),
            "{conflict}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(api.state().join("issue_create_count")).unwrap(),
        "1"
    );
}

#[tokio::test]
async fn issue_view_returns_state_title_and_body() {
    let _lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let api = Api::new();
    let fx = boot_fixture().await;
    let expected = issue(42, "CLOSED", "Requirements", "Full body");
    api.write("view.json", &expected);
    let response = call_tool(&fx, 3, VIEW, json!({"repo":"owner/repo","issue":42})).await;
    assert_eq!(stdout(&response), expected);
    let path = response["result"]["structuredContent"]["result"]["event"]["artifact_path"]
        .as_str()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&std::fs::read_to_string(path).unwrap()).unwrap(),
        expected
    );
}

#[tokio::test]
async fn issue_view_contract_version_does_not_replay_body_only_result() {
    let _lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let api = Api::new();
    let fx = boot_fixture().await;
    let args = json!({"repo":"owner/repo","issue":42});
    api.write("view.json", &issue(42, "OPEN", "Old", "Old"));
    let first = call_tool(&fx, 3, VIEW, args.clone()).await;
    let op = first["result"]["structuredContent"]["op_id"]
        .as_str()
        .unwrap();
    // Model a durable historical body-only receipt at each prior read contract key.
    let legacy_key = scoped_idem_key(
        PLUGIN_ID,
        &fx.track_id,
        &fx.card_id,
        "gh.issue.view:v2:owner/repo:42",
    );
    sqlx::query("UPDATE operations SET idempotency_key=?1, tx_output_json=json_set(tx_output_json,'$.result',json(?2)) WHERE id=?3")
        .bind(legacy_key)
        .bind(json!({"stdout":"Old body only"}).to_string())
        .bind(op)
        .execute(fx.repo.pool())
        .await
        .unwrap();
    let expected = issue(42, "CLOSED", "New", "Fresh body");
    api.write("view.json", &expected);
    let fresh = call_tool(&fx, 4, VIEW, args.clone()).await;
    assert_eq!(stdout(&fresh), expected);
    assert_ne!(fresh["result"]["structuredContent"]["op_id"], op);
    let attempted = call_tool(
        &fx,
        5,
        VIEW,
        json!({"repo":"owner/repo","issue":42,"attempt":"refresh"}),
    )
    .await;
    let attempt_op = attempted["result"]["structuredContent"]["op_id"]
        .as_str()
        .unwrap();
    let old = format!("gh.issue.view:v3:{}", json!(["owner/repo", 42, "refresh"]));
    sqlx::query("UPDATE operations SET idempotency_key=?1, tx_output_json=json_set(tx_output_json,'$.result',json(?2)) WHERE id=?3")
        .bind(scoped_idem_key(PLUGIN_ID, &fx.track_id, &fx.card_id, &old))
        .bind(json!({"stdout":"Old attempt body"}).to_string())
        .bind(attempt_op)
        .execute(fx.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        stdout(
            &call_tool(
                &fx,
                6,
                VIEW,
                json!({"repo":"owner/repo","issue":42,"attempt":"refresh"})
            )
            .await
        ),
        expected
    );
}

#[tokio::test]
async fn planner_issue_search_filters_and_refreshes() {
    let _lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let api = Api::new();
    let fx = boot_fixture().await;
    let caller = git_forge_track_worktree::issue_planner(&fx).await;
    let all = json!([
        issue(1, "OPEN", "needle", "A"),
        issue(2, "CLOSED", "needle", "B"),
        issue(3, "OPEN", "other", "C")
    ]);
    api.write("search.json", &all);
    let args = json!({"repo":"owner/repo","query":"needle"});
    let first = call(&fx, &caller, 3, SEARCH, args.clone()).await;
    assert_eq!(stdout(&first), json!([all[0], all[1]]));
    assert_eq!(
        stdout(
            &call(
                &fx,
                &caller,
                4,
                SEARCH,
                json!({"repo":"owner/repo","query":"needle","state":"closed","limit":1})
            )
            .await
        ),
        json!([all[1]])
    );
    api.write("search.json", &json!([]));
    let replay = call(&fx, &caller, 5, SEARCH, args).await;
    assert_eq!(
        replay["result"]["structuredContent"]["op_id"],
        first["result"]["structuredContent"]["op_id"]
    );
    assert_eq!(stdout(&replay), json!([all[0], all[1]]));
    let fresh = call(
        &fx,
        &caller,
        6,
        SEARCH,
        json!({"repo":"owner/repo","query":"needle","attempt":1}),
    )
    .await;
    assert_eq!(stdout(&fresh), json!([]));
    let path = fresh["result"]["structuredContent"]["result"]["event"]["artifact_path"]
        .as_str()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&std::fs::read_to_string(path).unwrap()).unwrap(),
        json!([])
    );
    for invalid in [
        json!({"state":"bad"}),
        json!({"limit":0}),
        json!({"limit":101}),
        json!({"limit":1.5}),
    ] {
        let mut args = json!({"repo":"owner/repo","query":"needle"});
        args.as_object_mut()
            .unwrap()
            .extend(invalid.as_object().unwrap().clone());
        let rejected = call(&fx, &caller, 7, SEARCH, args).await;
        assert!(
            rejected["result"]["isError"] == true || rejected["error"]["code"] == -32602,
            "{rejected}"
        );
    }
}
