//! A failed briefing read must retain its input without retrying every tick.
use super::completed_commit_tests::Fixture;
use super::*;
use crate::db::sqlite::append_decision_event_in_tx;

#[tokio::test]
async fn recovery_briefing_read_failure_retains_input_and_paces_retry() {
    let fx = Fixture::new().await;
    let inner = &fx.harness.inner;
    let event = Event::TaskExecutionSettled {
        task_id: "unavailable-retained-attempt".into(),
        operation_id: "retained-operation".into(),
    };
    let mut tx = fx.repo.pool().begin().await.unwrap();
    let id = append_decision_event_in_tx(
        &mut tx,
        &ActorId::KernelDispatcher,
        &harness_event_scope(inner, "task.execution_settled"),
        None,
        &event,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let original: String = sqlx::query_scalar("SELECT payload FROM events WHERE id=?1")
        .bind(id)
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    let notice = QueueEntry::system(
        crate::dispatcher::harness_observation_from_event(&inner.track_id, &event, None).unwrap(),
        Some(id),
    )
    .unwrap();
    let user =
        QueueEntry::user_message("Please explain the retained failure.".into(), None, vec![]);
    fx.enqueue(vec![notice, user]).await;
    let before = fx.stored().await;
    // A corrupt persisted event is a deterministic read failure at the actual
    // briefing boundary. Do not inject a failure into a hand-written renderer.
    sqlx::query("UPDATE events SET payload='{' WHERE id=?1")
        .bind(id)
        .execute(fx.repo.pool())
        .await
        .unwrap();
    let error = maybe_issue_turn(inner).await.unwrap_err();
    assert!(error.to_string().contains("EOF"), "{error}");
    assert_eq!(
        fx.stored().await.pending_entries(),
        before.pending_entries()
    );
    assert_eq!(inner.daemon.turn_start_count_for_test(), 0);
    // A second actual issue attempt must hit the existing pacing guard, not
    // repeat the broken read and the Issuing/Idle persistence cycle.
    let repeated = maybe_issue_turn(inner).await;
    // Restore the injected corruption before checking the regression assertion.
    sqlx::query("UPDATE events SET payload=?1 WHERE id=?2")
        .bind(original)
        .bind(id)
        .execute(fx.repo.pool())
        .await
        .unwrap();
    assert!(
        repeated.is_ok(),
        "unpaced briefing read repeated immediately: {repeated:?}"
    );
    assert!(
        fx.harness
            .issuance_block()
            .await
            .unwrap()
            .contains("recovery decision briefing")
    );
    assert_eq!(
        fx.stored().await.pending_entries(),
        before.pending_entries()
    );
    // The ordinary explicit retry path clears the delay after the read is fixed.
    fx.harness.retry_issuance_now().await;
    maybe_issue_turn(inner).await.unwrap();
    assert_eq!(inner.daemon.turn_start_count_for_test(), 1);
    let issued = fx.stored().await;
    assert!(issued.pending_entries().is_empty());
    assert!(
        issued
            .issued_input_segments
            .unwrap()
            .segments
            .iter()
            .any(|segment| segment
                .text
                .contains("Please explain the retained failure."))
    );
    assert!(fx.harness.issuance_block().await.is_none());
}

/// A settled legacy attempt is briefed with its actual executor route; the
/// isolated Codex envelope belongs only to isolated selections.
#[tokio::test]
async fn recovery_briefing_states_legacy_executor_route_not_isolated_envelope() {
    let fx = Fixture::new().await;
    let inner = &fx.harness.inner;
    let task_id = "legacy-terminal-attempt";
    sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,status_detail,created_at_ms,updated_at_ms) \
         VALUES(?1,?2,'legacy','terminal','false','null','failed','spawn-failed: controlled preparation failure',1,1)",
    )
    .bind(task_id)
    .bind(inner.track_id.as_str())
    .execute(fx.repo.pool())
    .await
    .unwrap();
    let event = Event::TaskExecutionSettled {
        task_id: task_id.into(),
        operation_id: "legacy-operation".into(),
    };
    let mut tx = fx.repo.pool().begin().await.unwrap();
    let id = append_decision_event_in_tx(
        &mut tx,
        &ActorId::KernelDispatcher,
        &harness_event_scope(inner, "task.execution_settled"),
        None,
        &event,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let notice = QueueEntry::system(
        crate::dispatcher::harness_observation_from_event(&inner.track_id, &event, None).unwrap(),
        Some(id),
    )
    .unwrap();
    fx.enqueue(vec![notice]).await;
    maybe_issue_turn(inner).await.unwrap();
    assert_eq!(inner.daemon.turn_start_count_for_test(), 1);
    let issued = fx.stored().await.issued_input_segments.unwrap();
    let text = issued
        .segments
        .iter()
        .find_map(|segment| {
            segment
                .text
                .split_once("Recovery decision briefing (kernel snapshot):\n")
                .map(|(_, rest)| {
                    rest.split_once("\nEnd recovery decision briefing.")
                        .unwrap()
                        .0
                })
        })
        .expect("legacy settlement must still be briefed");
    let brief: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(brief["attempt_id"], task_id);
    assert_eq!(
        brief["executor_environment"],
        serde_json::json!({
            "executor": "terminal",
            "note": "recovery re-runs on the same executor as the failed attempt; its environment is unchanged",
        })
    );
    let changes = brief["recover_changes"].as_str().unwrap();
    assert!(changes.contains("same executor as the failed attempt"));
    assert!(!changes.contains("only the workspace is new"));
}
