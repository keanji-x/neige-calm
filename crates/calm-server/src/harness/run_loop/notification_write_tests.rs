//! Provider notification storms must not acquire SQLite's single writer for unchanged state.
use super::completed_commit_tests::Fixture;
use super::*;
use crate::codex_appserver::Notification;
use crate::db::prelude::*;
use crate::harness::codex_events::planner_event;
use crate::model::CardRole;
use serde_json::json;

async fn deliver(f: &Fixture, method: &str, params: Value) {
    on_notification(
        &f.harness.inner,
        &f.harness.inner.live_claim.writer(),
        planner_event(Notification::parse(method.into(), params)),
    )
    .await
    .unwrap();
}

async fn audit_session_updates(f: &Fixture) {
    // A table-backed trigger sees every pooled connection, including updates whose JSON and
    // millisecond timestamp happen to equal the old value. This exists only in the fixture DB.
    sqlx::query("CREATE TABLE notification_write_audit (session_id TEXT NOT NULL)")
        .execute(f.repo.pool())
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER notification_session_updated AFTER UPDATE ON worker_sessions
         BEGIN INSERT INTO notification_write_audit(session_id) VALUES (NEW.id); END",
    )
    .execute(f.repo.pool())
    .await
    .unwrap();
}

async fn writes(f: &Fixture) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM notification_write_audit")
        .fetch_one(f.repo.pool())
        .await
        .unwrap()
}

async fn event_count(f: &Fixture) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(f.repo.pool())
        .await
        .unwrap()
}

async fn assert_storage_neutral_storm(role: CardRole) {
    let f = Fixture::for_role(role).await;
    audit_session_updates(&f).await;
    let before = f.stored().await;
    let events_before = event_count(&f).await;
    for _ in 0..32 {
        for method in [
            "item/reasoning/summaryTextDelta",
            "item/reasoning/textDelta",
            "item/futureProviderDelta",
            "future/providerNotification",
        ] {
            let params = json!({"threadId":"thread-commit-wake","turnId":"turn-1",
                "itemId":"reply-1","delta":"thinking"});
            assert!(matches!(
                planner_event(Notification::parse(method.into(), params.clone())).kind,
                PlannerEventKind::Ignored
            ));
            deliver(&f, method, params).await;
        }
        deliver(
            &f,
            "thread/status/changed",
            json!({"threadId":"thread-commit-wake","status":{"type":"active"}}),
        )
        .await;
        deliver(
            &f,
            "item/agentMessage/delta",
            json!({"threadId":"thread-commit-wake","turnId":"turn-1",
                "itemId":"reply-1","delta":"answer"}),
        )
        .await;
    }
    assert_eq!(
        writes(&f).await,
        0,
        "no-op frames must not update session rows"
    );
    assert_eq!(f.stored().await, before);
    assert_eq!(event_count(&f).await, events_before);
    assert!(
        f.repo
            .transcript_rows_of_thread(f.harness.inner.card_id.as_str(), "thread-commit-wake")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn planner_notification_storm_is_storage_neutral() {
    assert_storage_neutral_storm(CardRole::Planner).await;
}

#[tokio::test]
async fn assistant_notification_storm_is_storage_neutral() {
    assert_storage_neutral_storm(CardRole::Assistant).await;
}

#[tokio::test]
async fn semantic_notifications_still_persist_recoverable_state_and_final_records() {
    for role in [CardRole::Planner, CardRole::Assistant] {
        let f = Fixture::for_role(role).await;
        audit_session_updates(&f).await;
        deliver(
            &f,
            "turn/started",
            json!({"threadId":"thread-commit-wake","turn":{"id":"turn-1"}}),
        )
        .await;
        let running = f.stored().await;
        assert_eq!(running.phase, HarnessPhaseTag::TurnRunning);
        assert_eq!(running.last_turn_id.as_deref(), Some("turn-1"));
        assert!(matches!(
            state_from_runtime_header(&running.runtime_header(), &f.harness.inner.inputs),
            HarnessState::Resumed { .. }
        ));
        let started_writes = writes(&f).await;
        assert!(
            started_writes > 0,
            "the audit trigger must see real snapshot writes"
        );
        deliver(
            &f,
            "item/completed",
            json!({"threadId":"thread-commit-wake","turnId":"turn-1",
                "item":{"id":"reply-1","type":"agentMessage","text":"final answer"}}),
        )
        .await;
        deliver(
            &f,
            "turn/completed",
            json!({"threadId":"thread-commit-wake","turn":{"id":"turn-1","status":"completed"}}),
        )
        .await;
        let completed = f.stored().await;
        assert_eq!(completed.phase, HarnessPhaseTag::TurnCompleted);
        assert!(
            matches!(state_from_runtime_header(&completed.runtime_header(), &f.harness.inner.inputs), HarnessState::TurnCompleted { last_turn_id } if last_turn_id == "turn-1")
        );
        assert!(writes(&f).await > started_writes);
        let rows = f
            .repo
            .transcript_rows_of_thread(f.harness.inner.card_id.as_str(), "thread-commit-wake")
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.method == "item/completed"
            && serde_json::from_str::<Value>(&row.params).unwrap()["item"]["text"]
                == "final answer"));
        assert!(rows.iter().any(|row| row.method == "turn/completed"
            && serde_json::from_str::<Value>(&row.params).unwrap()["status"] == "completed"));
    }
}
