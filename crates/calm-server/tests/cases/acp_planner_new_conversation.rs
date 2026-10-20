//! #2774 — a new conversation on an OpenCode Planner: an event committed while the committed
//! start is `starting` and no request holds the card is delivered by the re-driven start's install.
use super::*;
use axum::http::Request;
use calm_server::event::{Event, EventScope};
use calm_server::ids::ActorId;
use calm_server::model::new_id;
use calm_server::test_seams::{PausePoint, install_pause_for_test};
use std::sync::Arc;
use tokio::sync::Notify;
use tower::ServiceExt;

const WAKE: &str = "committed while the new OpenCode conversation was starting";

/// Commit a Planner wake carrying `text` on the card's Track and broadcast it, as a writer does.
async fn wake(stack: &Stack, card: &str, text: &str) {
    let card = stack.repo().card_get(card).await.unwrap().unwrap();
    let track = stack
        .repo()
        .track_get(card.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    stack
        .repo()
        .log_pure_event(
            ActorId::Kernel,
            EventScope::Track {
                track: track.id.clone(),
                area: track.area_id,
            },
            None,
            &stack.state.events,
            &stack.state.card_role_cache,
            &stack.state.track_area_cache,
            Event::TrackWakeRequested {
                track_id: track.id,
                source: "test".into(),
                key: new_id(),
                text: text.into(),
            },
        )
        .await
        .unwrap();
}

fn prompts_with(root: &Root, text: &str) -> usize {
    requests(root, "session/prompt")
        .iter()
        .map(|frame| frame.to_string().matches(text).count())
        .sum()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn planner_new_conversation_an_opencode_event_before_the_redrive_arrives_once() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    assert_eq!(
        turn(&stack, &card, "settle", 1).await["status"],
        "completed"
    );
    let old = stack.runtime(&card).await;

    // Commit the cutover, then lose the request before the new session is minted.
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    install_pause_for_test(
        "conversation-start-prepared",
        &card,
        PausePoint {
            entered: entered.clone(),
            release: release.clone(),
        },
    );
    let pending = {
        let app = stack.app.clone();
        let request = Request::builder()
            .method("POST")
            .uri(format!("/api/cards/{card}/planner/conversations"))
            .header("x-calm-actor", "user")
            .header("idempotency-key", new_id())
            .body(axum::body::Body::empty())
            .unwrap();
        tokio::spawn(async move { app.oneshot(request).await })
    };
    tokio::time::timeout(Duration::from_secs(20), entered.notified())
        .await
        .expect("the cutover committed");
    pending.abort();
    let _ = pending.await;
    tokio::time::timeout(Duration::from_secs(20), async {
        while stack.state.planner_recovery_lock_handles_for_test(&card) != 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the lost request released the card");
    let carrier = stack.runtime(&card).await;
    assert_ne!(carrier.id, old.id, "premise: the cutover committed");
    assert!(
        stack.state.harness.get(&carrier.id).is_none(),
        "premise: nothing serves the starting row"
    );

    wake(&stack, &card, WAKE).await;
    // Let the dispatcher find the row `starting` before the start is re-driven.
    tokio::time::sleep(Duration::from_millis(300)).await;
    calm_server::recover_operations_on_boot(&stack.state)
        .await
        .unwrap();
    assert_eq!(stack.runtime(&card).await.id, carrier.id);
    tokio::time::timeout(Duration::from_secs(20), async {
        while prompts_with(&root, WAKE) == 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the event reaches the new conversation");
    // A second copy would follow within the debounce window.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(prompts_with(&root, WAKE), 1, "exactly once");
    stack.shutdown().await;
}
