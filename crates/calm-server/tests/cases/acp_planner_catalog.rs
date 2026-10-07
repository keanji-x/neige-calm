use super::*;
use tower::ServiceExt;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_catalog_before_creation_never_prompts_or_grants_mcp() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (status, catalog) = stack
        .send("GET", "/api/models?provider=opencode", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{catalog}");
    assert_eq!(catalog["source"], "live", "{catalog}");
    assert_eq!(catalog["default"]["model"], "fixture/model-a");
    assert!(
        catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["model"] == "fixture/model-b")
    );
    assert!(requests(&root, "session/prompt").is_empty());
    let created = requests(&root, "session/new");
    assert_eq!(created.len(), 1);
    assert_eq!(created[0]["params"]["mcpServers"], json!([]));
    assert_eq!(requests(&root, "session/close").len(), 1);
    assert!(
        !std::path::Path::new(&format!("/proc/{}", created[0]["_pid"])).exists(),
        "discovery must stop its process before returning"
    );
    let pool = stack.repo().sqlite_pool().unwrap();
    let submissions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM acp_submissions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(submissions, 0);
    for line in std::fs::read_to_string(root.path().join("environment.jsonl"))
        .unwrap()
        .lines()
    {
        let presence: Value = serde_json::from_str(line).unwrap();
        assert_eq!(presence["NEIGE_MCP_TOKEN"], false);
        assert_eq!(presence["NEIGE_MCP_SOCKET"], false);
        assert_eq!(presence["NEIGE_MCP_DAEMON_TOKEN"], false);
    }

    let (_, again) = stack
        .send("GET", "/api/models?provider=opencode", None)
        .await;
    assert_eq!(again, catalog, "repeated reads share the cached discovery");
    assert_eq!(requests(&root, "session/new").len(), 1);
    let (_, card) = stack
        .create_claude_track_with(json!({"planner_provider":"opencode","model":"fixture/model-b"}))
        .await;
    assert_eq!(
        turn(&stack, &card, "selected before creation", 1).await["status"],
        "completed"
    );
    assert_eq!(
        requests(&root, "session/set_config_option")[0]["params"]["value"],
        "fixture/model-b"
    );
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_catalog_before_first_input_supports_selected_first_turn() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (track, card) = create(&stack).await;
    let before = stack.runtime(&card).await;
    let (status, catalog) = stack
        .send("GET", &format!("/api/models?card_id={card}"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{catalog}");
    assert_eq!(catalog["source"], "live", "{catalog}");
    let workspace = stack
        .repo()
        .track_get(&track)
        .await
        .unwrap()
        .unwrap()
        .workspace
        .agent_cwd()
        .to_owned();
    assert_eq!(
        requests(&root, "session/new")[0]["params"]["cwd"],
        workspace
    );
    assert_eq!(stack.runtime(&card).await.session_id, before.session_id);
    assert!(requests(&root, "session/prompt").is_empty());
    let (status, body) = stack
        .send(
            "PUT",
            &format!("/api/cards/{card}/planner/model"),
            Some(json!({"model":"fixture/model-b","reasoning_effort":null})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        turn(&stack, &card, "use selected model", 1).await["status"],
        "completed"
    );
    let changes = requests(&root, "session/set_config_option");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["params"]["configId"], "declared-model-key");
    assert_eq!(changes[0]["params"]["value"], "fixture/model-b");
    let (_, catalog) = stack
        .send("GET", &format!("/api/models?card_id={card}"), None)
        .await;
    assert_eq!(
        catalog["default"]["model"], "fixture/model-b",
        "the real session replaces the preview"
    );
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_catalog_concurrent_queries_share_discovery_and_workspace_changes_recheck() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (a, b) = tokio::join!(
        stack.send("GET", "/api/models?provider=opencode", None),
        stack.send("GET", "/api/models?provider=opencode", None)
    );
    assert_eq!(a.0, StatusCode::OK);
    assert_eq!(a.1["source"], "live");
    assert_eq!(a, b);
    assert_eq!(requests(&root, "session/new").len(), 1);
    let (_, card) = create(&stack).await;
    let (_, catalog) = stack
        .send("GET", &format!("/api/models?card_id={card}"), None)
        .await;
    assert_eq!(catalog["source"], "live");
    let created = requests(&root, "session/new");
    assert_eq!(created.len(), 2);
    assert_ne!(created[0]["params"]["cwd"], created[1]["params"]["cwd"]);
    assert!(requests(&root, "session/prompt").is_empty());
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_catalog_failure_is_cached_and_process_is_stopped() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    std::fs::write(root.path().join("scenario"), "catalog_error").unwrap();
    let (status, catalog) = stack
        .send("GET", "/api/models?provider=opencode", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalog["source"], "unavailable");
    assert_eq!(catalog["models"], json!([]));
    let created = requests(&root, "session/new");
    assert_eq!(created.len(), 1);
    assert!(!std::path::Path::new(&format!("/proc/{}", created[0]["_pid"])).exists());
    let (_, again) = stack
        .send("GET", "/api/models?provider=opencode", None)
        .await;
    assert_eq!(again, catalog);
    assert_eq!(requests(&root, "session/new").len(), 1);
    assert!(requests(&root, "session/prompt").is_empty());
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_catalog_does_not_require_optional_close_capability() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    std::fs::write(root.path().join("scenario"), "catalog_no_close").unwrap();
    let (_, catalog) = stack
        .send("GET", "/api/models?provider=opencode", None)
        .await;
    assert_eq!(catalog["source"], "live");
    assert!(requests(&root, "session/close").is_empty());
    assert!(requests(&root, "session/prompt").is_empty());
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_catalog_cancellation_stops_marked_descendants_without_another_request() {
    cancelled_catalog_stops_children("catalog_cancel").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_catalog_cancellation_during_initialize_stops_marked_descendants() {
    cancelled_catalog_stops_children("catalog_cancel_initialize").await;
}

async fn cancelled_catalog_stops_children(scenario: &str) {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    std::fs::write(root.path().join("scenario"), scenario).unwrap();
    let app = stack.app.clone();
    let query = tokio::spawn(async move {
        app.oneshot(
            axum::http::Request::builder()
                .uri("/api/models?provider=opencode")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
    });
    wait_file(&root, "catalog-helper").await;
    let pid: i32 = std::fs::read_to_string(root.path().join("catalog-helper"))
        .unwrap()
        .parse()
        .unwrap();
    let instance = calm_server::planner_process::MarkerInstance::for_data_dir(
        &root.data_dir(),
        calm_server::acp_planner::config::MARKER_KEY,
    )
    .unwrap();
    let marker = instance.marker("catalog");
    assert!(calm_server::proc_identity::proc_env_contains(
        pid,
        calm_server::acp_planner::config::MARKER_KEY,
        &marker
    ));
    query.abort();
    assert!(query.await.unwrap_err().is_cancelled());
    let stopped = tokio::time::timeout(Duration::from_secs(3), async {
        while calm_server::proc_identity::proc_env_contains(
            pid,
            calm_server::acp_planner::config::MARKER_KEY,
            &marker,
        ) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    // Clean the deliberately orphaned helper even when this reproduction is red.
    calm_server::planner_process::stop(&instance, "catalog")
        .await
        .unwrap();
    assert!(
        stopped.is_ok(),
        "cancelled catalog discovery left its marked helper running"
    );
    stack.shutdown().await;
}
