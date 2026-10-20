//! Creation policy through real registration reads and POST /api/tracks.
use super::*;

async fn stored_mode(stack: &Stack, track: &str, card: &str) -> Value {
    stack
        .repo()
        .cards_by_track(track)
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.id.as_str() == card)
        .unwrap()
        .payload["permission_mode"]
        .clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_declared_mode_is_adopted_by_new_planners() {
    for mode in ["full", "ask", "never"] {
        let root = Root::new("unused");
        let stack =
            boot_as_with_creation_mode(&root, "opencode", json!({}), "1.0.0", Some(mode)).await;
        let (track, card) = create(&stack).await;
        assert_eq!(stored_mode(&stack, &track, &card).await, json!(mode));
        stack.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_undeclared_mode_and_codex_keep_never() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (track, card) = create(&stack).await;
    assert_eq!(stored_mode(&stack, &track, &card).await, json!("never"));
    stack.shutdown().await;

    let root = Root::new("unused");
    let stack =
        boot_as_with_creation_mode(&root, "opencode", json!({}), "1.0.0", Some("full")).await;
    let (track, card) = stack
        .create_claude_track_with(json!({"planner_provider":"codex"}))
        .await;
    assert_eq!(stored_mode(&stack, &track, &card).await, json!("never"));
    stack.shutdown().await;
}
