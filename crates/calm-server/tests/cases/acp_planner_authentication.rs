//! Native ACP authentication stays independent of the shared Codex daemon.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_does_not_inherit_codex_authentication_hold_or_notice() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let daemon = &stack.state.shared_codex_appserver;
    daemon.emit_notification_for_test(calm_server::codex_appserver::Notification::Other {
        method: "error".into(), params: json!({"error":{"message":"Your access token could not be refreshed because your refresh token was already used.","codexErrorInfo":"unauthorized"}}),
    });
    assert!(daemon.authentication_hold().is_some());
    let (_, card) = create(&stack).await;
    assert_eq!(
        turn(&stack, &card, "independent native authentication", 1).await["status"],
        "completed"
    );
    let (status, providers) = stack.send("GET", "/api/agent-providers", None).await;
    assert_eq!(status, StatusCode::OK);
    let providers = providers.as_array().unwrap();
    assert!(
        providers
            .iter()
            .all(|p| p.get("authentication_method") == Some(&Value::Null))
    );
    let acp = providers
        .iter()
        .find(|p| p["provider"] == "opencode")
        .unwrap();
    let codex = providers.iter().find(|p| p["provider"] == "codex").unwrap();
    assert!(acp["authentication_notice"].is_null());
    assert!(!codex["authentication_notice"].is_null());
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    stack.shutdown().await;
}

/// OpenCode's profile names no authentication method: even an agent that offers some gets no
/// `authenticate`, and a turn's process sees exactly the frames it saw before.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_opencode_turn_sends_no_authenticate_when_the_agent_offers_methods() {
    let root = Root::new("unused");
    let stack = boot_with(
        &root,
        json!({"ACP_FIXTURE_AUTH_METHODS":"opencode-login"}),
        "1.0.0",
    )
    .await;
    let (_, card) = create(&stack).await;
    assert_eq!(
        turn(&stack, &card, "unchanged frames", 1).await["status"],
        "completed"
    );
    let frames: Vec<Value> = std::fs::read_to_string(root.path().join("requests.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        frames.iter().all(|frame| frame["method"] != "authenticate"),
        "{frames:?}"
    );
    let prompt = frames
        .iter()
        .find(|frame| frame["method"] == "session/prompt")
        .unwrap();
    let turn_frames: Vec<&Value> = frames
        .iter()
        .filter(|frame| frame["_pid"] == prompt["_pid"] && frame["method"].is_string())
        .map(|frame| &frame["method"])
        .collect();
    assert_eq!(
        turn_frames,
        [
            &json!("initialize"),
            &json!("session/new"),
            &json!("session/prompt")
        ]
    );
    stack.shutdown().await;
}

/// An agent reporting a version below the registered minimum is unavailable, and says why.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_below_the_minimum_version_reads_unavailable() {
    let root = Root::new("unused");
    let stack = boot_with(&root, json!({"ACP_FIXTURE_VERSION":"2.1.3"}), "2.1.4").await;
    let (status, providers) = stack.send("GET", "/api/agent-providers", None).await;
    assert_eq!(status, StatusCode::OK);
    let acp = providers
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["provider"] == "opencode")
        .unwrap();
    assert_eq!(acp["status"], "unavailable", "{acp}");
    assert!(
        acp["reason"]
            .as_str()
            .unwrap()
            .contains("version 2.1.4 or later"),
        "{acp}"
    );
    assert!(requests(&root, "session/new").is_empty());
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_opencode_profile_refuses_unsupported_versions_even_with_old_operator_minimum() {
    for version in ["1.18.35", "2.1.2", "2.1.3-beta.1", "3.0.0", "3.0.0-beta.1"] {
        let root = Root::new("unused");
        let stack = boot_with(&root, json!({"ACP_FIXTURE_VERSION":version}), "1.0.0").await;
        let (_, providers) = stack.send("GET", "/api/agent-providers", None).await;
        let acp = providers
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["provider"] == "opencode")
            .unwrap();
        assert_eq!(acp["status"], "unavailable", "{version}: {acp}");
        assert!(
            acp["reason"]
                .as_str()
                .unwrap()
                .contains("OpenCode requires >=2.1.3,<3.0.0; install @opencode/cli"),
            "{acp}"
        );
        assert!(requests(&root, "session/new").is_empty());
        stack.shutdown().await;
    }
}

fn workspace_ready_path(root: &Root, method: &str) -> std::path::PathBuf {
    let frames = requests(root, method);
    let pid = &frames.last().expect("native attachment or prompt")["_pid"];
    std::fs::read_to_string(root.path().join("environment.jsonl"))
        .unwrap()
        .lines()
        .find_map(|line| {
            let launch: Value = serde_json::from_str(line).unwrap();
            if &launch["_pid"] != pid {
                return None;
            }
            let config: Value =
                serde_json::from_str(launch["opencode_config_content"].as_str()?).unwrap();
            config["plugins"][0]["options"]["ready"]
                .as_str()
                .map(Into::into)
        })
        .expect("the invoking native process owns its plugin readiness path")
}

async fn wait_workspace_hook_refusal(stack: &Stack, card: &str) {
    let worker = stack.runtime(card).await.id;
    let handle = stack.harness(&worker);
    tokio::time::timeout(Duration::from_secs(20), async {
        while handle.refused_issuances_for_test() == 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("missing workspace hook must explicitly refuse issuance");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opencode_workspace_hook_missing_at_attachment_refuses_before_prompt() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    std::fs::write(root.path().join("workspace-hook-not-ready"), "").unwrap();
    let (status, body) = stack.post_input(&card, "requires workspace tools").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_workspace_hook_refusal(&stack, &card).await;
    assert_eq!(requests(&root, "session/new").len(), 1);
    assert!(requests(&root, "session/prompt").is_empty());
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opencode_workspace_hook_teardown_refuses_a_warm_process() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    assert_eq!(turn(&stack, &card, "first", 1).await["status"], "completed");
    let ready = workspace_ready_path(&root, "session/prompt");
    assert!(ready.exists());
    std::fs::remove_file(&ready).unwrap();
    let (status, body) = stack.post_input(&card, "after hook teardown").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_workspace_hook_refusal(&stack, &card).await;
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opencode_workspace_hook_restart_never_reuses_a_previous_activation() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    assert_eq!(
        turn(&stack, &card, "before restart", 1).await["status"],
        "completed"
    );
    let old = workspace_ready_path(&root, "session/prompt");
    assert!(old.exists());
    stack.shutdown().await;
    assert!(!old.parent().unwrap().exists());
    let stack = boot(&root).await;
    std::fs::write(root.path().join("workspace-hook-not-ready"), "").unwrap();
    let (status, body) = stack.post_input(&card, "after restart").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_workspace_hook_refusal(&stack, &card).await;
    let new = workspace_ready_path(&root, "session/resume");
    assert_ne!(old, new);
    assert!(!new.exists());
    assert_eq!(requests(&root, "session/resume").len(), 1);
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opencode_workspace_hook_missing_makes_discovery_unavailable() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    std::fs::write(root.path().join("workspace-hook-not-ready"), "").unwrap();
    let (status, catalog) = stack
        .send("GET", "/api/models?provider=opencode", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalog["source"], "unavailable");
    assert_eq!(requests(&root, "session/new").len(), 1);
    assert!(requests(&root, "session/prompt").is_empty());
    stack.shutdown().await;
}
