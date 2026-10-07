//! Real CLI authorization from the native process, including loaded sessions.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_cli_uses_the_current_card_context_and_rotates_it_on_load() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (track, card) = create(&stack).await;
    let worker = stack.runtime(&card).await.id;
    std::fs::write(root.path().join("scenario"), "cli").unwrap();
    for n in 1..=2 {
        assert_eq!(
            turn(&stack, &card, "CLI check", n).await["status"],
            "completed"
        );
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
                .fetch_one(&stack.repo().sqlite_pool().unwrap())
                .await
                .unwrap();
        assert!(hash.is_none(), "a settled turn cannot keep CLI authority");
    }
    assert_eq!(requests(&root, "session/new").len(), 1);
    assert_eq!(requests(&root, "session/load").len(), 1);
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
                    .fetch_one(&stack.repo().sqlite_pool().unwrap())
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
