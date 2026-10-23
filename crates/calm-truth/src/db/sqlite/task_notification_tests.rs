use super::*;
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use calm_types::event::PlannerNotification;

fn failure(notification: PlannerNotification) -> Event {
    Event::TaskFailed {
        notification,
        idempotency_key: "notification-test".into(),
        reason: "failure".into(),
        details: None,
        agent_message: None,
    }
}

#[tokio::test]
async fn task_notification_legacy_rejected_before_any_batch_insert() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let actor = ActorId::KernelDispatcher;
    let scope = EventScope::System;
    assert!(
        append_decision_events_in_tx(
            &mut tx,
            &actor,
            &scope,
            None,
            &[
                failure(PlannerNotification::Notify),
                failure(PlannerNotification::Legacy)
            ]
        )
        .await
        .is_err()
    );
    // Callers are permitted to commit other work after a rejected append.
    tx.commit().await.unwrap();
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(after, before);
}

#[tokio::test]
async fn task_notification_legacy_rejected_before_single_insert() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    assert!(
        append_decision_event_in_tx(
            &mut tx,
            &ActorId::KernelDispatcher,
            &EventScope::System,
            None,
            &failure(PlannerNotification::Legacy)
        )
        .await
        .is_err()
    );
    tx.commit().await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}
