//! A resident OpenCode process holds no ambient card credentials.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resident_opencode_accepts_mcp_calls_only_from_the_credential_owner() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    std::fs::write(root.path().join("scenario"), "mcp-origin").unwrap();
    assert_eq!(
        turn(&stack, &card, "check Neige tools", 1).await["status"],
        "completed"
    );

    let launches = std::fs::read_to_string(root.path().join("environment.jsonl")).unwrap();
    let resident = launches
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|launch| launch["opencode_config_content"].is_string())
        .expect("resident launch");
    assert_eq!(resident["NEIGE_MCP_TOKEN"], false);
    assert_eq!(resident["NEIGE_MCP_SOCKET"], false);

    let replies: [Value; 3] = serde_json::from_slice(
        &std::fs::read(root.path().join("mcp-origin-results.json")).unwrap(),
    )
    .unwrap();
    assert!(replies[0].get("result").is_some(), "{replies:?}");
    assert_ne!(replies[0]["result"]["isError"], true, "{replies:?}");
    for reply in &replies[1..] {
        assert_eq!(reply["result"]["isError"], true, "{reply}");
        assert_eq!(
            reply["result"]["structuredContent"]["code"], -32602,
            "{reply}"
        );
    }
    stack.shutdown().await;
}
