//! Observe attachment identity through REST, Harness recovery and real ACP setup frames.
use super::acp_compact::{compact, snapshot, turn, wait_phase};
use super::*;
use std::collections::HashSet;

async fn boot_antigravity(root: &Root) -> Stack {
    boot_as(
        root,
        "antigravity",
        json!({
            "ACP_FIXTURE_AGENT_NAME":"Antigravity ACP Server", "ACP_FIXTURE_VERSION":"1.5.0",
            "ACP_FIXTURE_CAPTURE_SHARED_TOKEN":"1"
        }),
        "1.5.0",
    )
    .await
}
fn token(root: &Root, native: &str) -> String {
    std::fs::read_to_string(root.path().join(format!("{native}.test-token"))).unwrap()
}
async fn detach(stack: &Stack, card: &str) {
    let runtime = stack.runtime(card).await;
    stack
        .state
        .harness
        .remove(&runtime.id)
        .expect("registered card")
        .shutdown()
        .await
        .unwrap();
}
async fn compact_completed(stack: &Stack, root: &Root, card: &str) {
    std::fs::write(root.path().join("scenario"), "shared-compact-success").unwrap();
    let (status, body) = compact(stack, card).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_phase(stack, &stack.runtime(card).await.id, "turn_completed").await;
    let state = snapshot(stack, card).await;
    assert_eq!(state["compact_outcome"]["status"], "completed", "{state}");
    std::fs::write(root.path().join("scenario"), "reply").unwrap();
}
async fn rotated(stack: &Stack, root: &Root, native: &str, old: &str) -> String {
    let fresh = token(root, native);
    assert_ne!(fresh, old);
    assert!(stack.token_authenticates(&fresh).await);
    assert!(!stack.token_authenticates(old).await);
    fresh
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn antigravity_mcp_rebind_and_rebuild_preserve_card_identity_and_rotate_credentials() {
    let root = Root::new("unused");
    let stack = boot_antigravity(&root).await;
    let (first_track, first) = stack
        .create_canonical_track_with(json!({"planner_provider":"antigravity"}))
        .await;
    let (_, second) = stack
        .create_canonical_track_with(json!({"planner_provider":"antigravity"}))
        .await;
    for card in [&first, &second] {
        assert_eq!(turn(&stack, card, "first", 1).await["status"], "completed");
    }
    let native = stack.runtime(&first).await.session_id.unwrap();
    let sibling_native = stack.runtime(&second).await.session_id.unwrap();
    let original = token(&root, &native);
    let sibling_token = token(&root, &sibling_native);
    assert!(stack.token_authenticates(&original).await);
    detach(&stack, &first).await;
    assert!(!stack.token_authenticates(&original).await);
    assert_eq!(
        turn(&stack, &first, "rebind", 2).await["status"],
        "completed"
    );
    let rebound = rotated(&stack, &root, &native, &original).await;
    assert!(stack.token_authenticates(&sibling_token).await);
    // Retire only the provider attachment through its production lifecycle owner.
    // Keep the live canonical Harness required by the human compact route.
    calm_server::acp_planner::config::sweep_track(
        stack.repo(),
        &stack.state.planner_wiring().acp,
        &first_track,
    )
    .await
    .unwrap();
    compact_completed(&stack, &root, &first).await;
    let compact_token = rotated(&stack, &root, &native, &rebound).await;
    let setups: Vec<_> = requests(&root, "session/new")
        .into_iter()
        .filter(|request| {
            !request["params"]["mcpServers"]
                .as_array()
                .unwrap()
                .is_empty()
        })
        .collect();
    assert_eq!(setups.len(), 2);
    let first_pid = setups[0]["_pid"].clone();
    assert!(setups.iter().all(|r| r["_pid"] == first_pid));
    assert!(
        requests(&root, "session/resume")
            .iter()
            .all(|r| r["_pid"] == first_pid)
    );
    stack.shutdown().await;

    let stack = boot_antigravity(&root).await;
    assert_eq!(
        turn(&stack, &first, "after rebuild", 3).await["status"],
        "completed"
    );
    let fresh = rotated(&stack, &root, &native, &compact_token).await;
    compact_completed(&stack, &root, &first).await;
    assert_eq!(
        stack.runtime(&first).await.session_id.as_deref(),
        Some(native.as_str())
    );
    assert_eq!(
        token(&root, &native),
        fresh,
        "an attached turn reuses its grant"
    );
    let resumes = requests(&root, "session/resume");
    assert_eq!(
        resumes.len(),
        3,
        "each detached attachment must actually resume"
    );
    assert_ne!(resumes.last().unwrap()["_pid"], first_pid);
    let setups: Vec<_> = setups.into_iter().chain(resumes).collect();
    let sibling_key = setups[1]["params"]["mcpServers"][0]["name"]
        .as_str()
        .unwrap();
    let mut names = HashSet::new();
    for setup in std::iter::once(&setups[0]).chain(setups.iter().skip(2)) {
        let server = &setup["params"]["mcpServers"][0];
        let name = server["name"].as_str().unwrap();
        assert_ne!(name, sibling_key, "different cards have isolated MCP names");
        let args = server["args"].as_array().unwrap();
        let budget = args
            .iter()
            .position(|arg| arg == "--max-tool-name-bytes")
            .unwrap();
        assert_eq!(
            args[budget + 1].as_str().unwrap().parse::<usize>().unwrap()
                + name.len()
                + "mcp__".len(),
            64
        );
        names.insert(name);
    }
    assert_eq!(
        names.len(),
        1,
        "rebinding must not accumulate MCP server names: {names:?}"
    );
    stack.shutdown().await;
}
