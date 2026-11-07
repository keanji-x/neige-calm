//! Real CLI authorization from the native process, including loaded sessions.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_cli_uses_the_current_card_context_and_rotates_it_on_load() {
    verify_cli_context("opencode").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn canonical_antigravity_cli_uses_fresh_authority_on_resume() {
    verify_cli_context("antigravity").await;
}

async fn verify_cli_context(provider: &str) {
    let root = Root::new("unused");
    let stack = if provider == "antigravity" {
        boot_as(
            &root,
            provider,
            json!({"ACP_FIXTURE_AGENT_NAME":"Antigravity ACP Server",
            "ACP_FIXTURE_VERSION":"1.5.3"}),
            "1.5.1",
        )
        .await
    } else {
        boot(&root).await
    };
    let (track, card) = if provider == "antigravity" {
        stack
            .create_canonical_track_with(json!({"planner_provider":provider}))
            .await
    } else {
        create(&stack).await
    };
    let worker = stack.runtime(&card).await.id;
    std::fs::write(root.path().join("scenario"), "cli").unwrap();
    for n in 1..=2 {
        let outcome = if provider == "antigravity" {
            super::acp_compact::turn(&stack, &card, "CLI check", n).await
        } else {
            turn(&stack, &card, "CLI check", n).await
        };
        assert_eq!(outcome["status"], "completed");
        let records = std::fs::read_to_string(root.path().join("cli-results.jsonl")).unwrap();
        let record: Value = serde_json::from_str(records.lines().last().unwrap()).unwrap();
        assert_eq!(
            record["help_exit"], 0,
            "the real CLI must authenticate: {record}"
        );
        assert_eq!(record["status_exit"], 0, "{record}");
        assert!(
            record["status"].to_string().contains(&track),
            "CLI must see this track: {record}"
        );
        assert_eq!(record["matches_mcp_context"], true);
        if n == 2 {
            assert_eq!(record["token_rotated"], true);
        }
        let hash: Option<String> =
            sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
                .bind(&worker)
                .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
                .await
                .unwrap();
        assert!(hash.is_none(), "a settled turn cannot keep CLI authority");
    }
    // Read-only model discovery also creates a native session, without this card's MCP context.
    let owned_attachments = |method| {
        requests(&root, method)
            .into_iter()
            .filter(|request| {
                request["params"]["mcpServers"]
                    .as_array()
                    .is_some_and(|servers| !servers.is_empty())
            })
            .count()
    };
    assert_eq!(owned_attachments("session/new"), 1);
    assert_eq!(owned_attachments("session/resume"), 1);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_native_identity_revokes_the_operational_cli_context() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    let worker = stack.runtime(&card).await.id;
    std::fs::write(root.path().join("scenario"), "wrong-identity").unwrap();
    let _ = stack.post_input(&card, "reject operational identity").await;
    wait_file(&root, "wrong-identity.json").await;
    let observed: Value =
        serde_json::from_slice(&std::fs::read(root.path().join("wrong-identity.json")).unwrap())
            .unwrap();
    assert_eq!(
        observed["token_present"], true,
        "failure must exercise an issued CLI context"
    );
    assert_eq!(observed["socket_present"], true);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let hash: Option<String> =
                sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
                    .bind(&worker)
                    .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
                    .await
                    .unwrap();
            if hash.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("failed launch must revoke its issued context");
    assert!(requests(&root, "session/prompt").is_empty());
    stack.shutdown().await;
}
