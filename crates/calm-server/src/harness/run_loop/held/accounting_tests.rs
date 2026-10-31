//! Actual held ownership and clock creation, independent of actor sampling frequency.
use super::*;
use crate::harness::run_loop::completed_commit_tests::Fixture;
use crate::harness::run_loop::{PlannerEvent, PlannerEventKind, on_notification};
use std::sync::Arc;
use std::time::Duration;

struct Refuse;
impl HeldResponder for Refuse {
    fn respond(self: Box<Self>, _: usize) {}
}
async fn open(inner: &Inner, key: &str) -> i64 {
    on_message(
        inner,
        HeldRequestMessage::Open {
            request_key: RequestKey(key.into()),
            connection: ConnectionId("accounting".into()),
            questions: vec![AskQuestion {
                title: "Continue?".into(),
                options: vec!["Yes".into(), "No".into()],
            }],
            responder: Box::new(Refuse),
        },
    )
    .await;
    *inner.held_requests.ask_ids().iter().max().unwrap()
}
async fn close(inner: &Inner, key: &str) {
    on_message(
        inner,
        HeldRequestMessage::Gone {
            request_key: RequestKey(key.into()),
        },
    )
    .await;
}
async fn start(inner: &Arc<Inner>, turn: &str) {
    on_notification(
        inner,
        &inner.live_claim.writer(),
        PlannerEvent {
            thread_id: inner.thread_id.read().await.clone(),
            kind: PlannerEventKind::TurnStarted {
                turn_id: turn.into(),
            },
        },
    )
    .await
    .unwrap();
}
async fn clock(inner: &Inner) -> Instant {
    match &*inner.state.lock().await {
        HarnessState::TurnRunning { watchdog_from, .. } => *watchdog_from,
        state => panic!("expected real running clock: {state:?}"),
    }
}

#[tokio::test]
async fn held_intervals_coalesced_between_passes_are_accounted_once() {
    let fx = Fixture::new().await;
    let inner = &fx.harness.inner;
    start(inner, "coalesced").await;
    // Age the real running clock far enough for the full actual-held interval;
    // the controlled table rewind represents provider wait, not a timer tick.
    fx.harness
        .rewind_turn_clock_for_test(Duration::from_secs(30))
        .await;
    let before = clock(inner).await;
    let started = Instant::now();
    let first = open(inner, "first").await;
    inner
        .held_requests
        .rewind_active_for_test(Duration::from_secs(10));
    let second = open(inner, "overlap").await;
    close(inner, "first").await;
    assert!(!inner.held_requests.contains(first));
    assert!(inner.held_requests.contains(second));
    close(inner, "overlap").await;
    let total = inner.held_requests.time().total;
    assert!(total >= Duration::from_secs(10));
    assert!(total <= Duration::from_secs(10) + started.elapsed());
    pause_watchdog_while_held(inner).await;
    let after = clock(inner).await;
    assert_eq!(after.duration_since(before), total);
    pause_watchdog_while_held(inner).await;
    assert_eq!(
        clock(inner).await,
        after,
        "closed interval must not be counted twice"
    );
}

#[tokio::test]
async fn new_running_clock_never_inherits_a_closed_prerunning_hold() {
    let fx = Fixture::new().await;
    let inner = &fx.harness.inner;
    open(inner, "before-start").await;
    inner
        .held_requests
        .rewind_active_for_test(Duration::from_secs(3600));
    close(inner, "before-start").await;
    start(inner, "new-clock").await;
    let before = clock(inner).await;
    pause_watchdog_while_held(inner).await;
    assert_eq!(
        clock(inner).await,
        before,
        "pre-running wait belongs to no running budget"
    );
    // A repeated start for this running clock must preserve unaccounted current wait.
    open(inner, "same-clock").await;
    inner
        .held_requests
        .rewind_active_for_test(Duration::from_secs(10));
    let accounted = *inner.watchdog_held_time.lock().unwrap();
    start(inner, "new-clock").await;
    assert_eq!(*inner.watchdog_held_time.lock().unwrap(), accounted);
    close(inner, "same-clock").await;
    on_notification(
        inner,
        &inner.live_claim.writer(),
        PlannerEvent {
            thread_id: inner.thread_id.read().await.clone(),
            kind: PlannerEventKind::TurnCompleted {
                turn: serde_json::json!({"id":"new-clock","status":"completed"}),
            },
        },
    )
    .await
    .unwrap();
    assert_eq!(
        *inner.watchdog_held_time.lock().unwrap(),
        inner.held_requests.time().total
    );
}
