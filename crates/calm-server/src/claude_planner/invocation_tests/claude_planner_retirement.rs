#![cfg(test)]
use super::claude_planner_stack_fixture::{Root, Stack};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_harness_tick_after_shutdown_never_mints_over_the_replacer() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_track, card_id) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card_id).await;
    let stale = stack.state.harness.remove(&runtime.id).expect("installed");
    let stale_session = stale.claude_session_for_test().expect("a Claude harness");
    stale.shutdown().await.expect("shutdown before any mint");

    // The replacer: the next input's registry miss respawns the harness, which mints.
    let (_, token) = stack.run_turn(&root, &card_id, "exit", "replacer").await;
    assert!(stack.token_authenticates(&token).await);
    let card_hash = card_token_hash(&stack, &card_id).await;

    let thread = runtime.thread_id.clone().expect("thread");
    let refused = stale_session
        .start_turn(
            &thread,
            provider::control::TurnRequest {
                input: provider::control::InputRequest {
                    items: vec![provider::InputItem::text("stale")],
                    correlation: provider::control::CorrelationKey(
                        uuid::Uuid::new_v4().simple().to_string(),
                    ),
                },
                selection: crate::planner_model::TurnModelSelection::inherit(),
                permission: crate::planner_permission_mode::PlannerPermissionMode::Never,
                edit: provider::control::TurnEdit::NoEdit,
            },
        )
        .await;
    assert!(refused.is_err(), "a shut-down session never starts a turn");
    assert!(
        stack.token_authenticates(&token).await,
        "the replacer's token stands"
    );
    assert_eq!(card_token_hash(&stack, &card_id).await, card_hash);
}

async fn card_token_hash(stack: &Stack, card_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT hashed_token FROM card_mcp_tokens WHERE card_id = ?1")
        .bind(card_id)
        .fetch_optional(stack.repo().sqlite_db().expect("pool").read_pool())
        .await
        .expect("card token read")
}
