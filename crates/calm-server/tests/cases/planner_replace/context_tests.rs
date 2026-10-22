//! Persisted context compatibility and refusal boundaries.
use super::*;

/// Only the dedicated regenerable initial context is disposable. Text and a missing origin
/// never grant this property to warnings, observations, or legacy System segments.
#[tokio::test]
async fn context_usage_replace_refuses_nonregenerable_context_even_with_a_user_message() {
    for context in [
        json!({"presentation":"system","text":"[回合预算]","attachments":[]}),
        json!({"presentation":"system","text":"budget reminder","attachments":[]}),
        json!({"presentation":"system","text":"previous timeout","attachments":[]}),
        json!({"presentation":"system_task_completed","text":"result","attachments":[],"origin":{"observation":"task_completed","event_id":42}}),
        json!({"presentation":"system_planner_turn_budget","text":"budget","attachments":[],"origin":{"observation":"system_context","event_id":null}}),
        json!({"presentation":"system_planner_context_usage","text":"usage","attachments":[],"origin":{"observation":"system_context","event_id":null}}),
    ] {
        let boot = two_turns().await;
        let (id, raw): (i64, String) = sqlx::query_as(
            "SELECT id,input_segments FROM harness_items WHERE card_id=?1 AND turn_id=?2 \
             AND input_segments IS NOT NULL ORDER BY id LIMIT 1",
        )
        .bind(boot.planner_card.id.as_str())
        .bind(TURN_B)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
        let mut segments: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(segments[0]["presentation"], "system_planner_turn_budget");
        assert_eq!(segments[1]["presentation"], "system_planner_context_usage");
        segments[0] = context;
        sqlx::query("UPDATE harness_items SET input_segments=?1 WHERE id=?2")
            .bind(segments.to_string())
            .bind(id)
            .execute(boot.repo.pool())
            .await
            .unwrap();
        assert_refused_unchanged(&boot, TURN_B, "system update").await;
        boot.harness.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn replace_keeps_legacy_all_user_rows_replaceable() {
    let boot = two_turns().await;
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id,input_segments FROM harness_items WHERE card_id=?1 AND turn_id=?2 AND input_segments IS NOT NULL")
        .bind(boot.planner_card.id.as_str()).bind(TURN_B).fetch_all(boot.repo.pool()).await.unwrap();
    assert!(!rows.is_empty());
    for (id, raw) in rows {
        let segments: Vec<Value> = serde_json::from_str::<Vec<Value>>(&raw)
            .unwrap()
            .into_iter()
            .filter(|segment| segment["presentation"] == "user")
            .collect();
        assert!(!segments.is_empty());
        sqlx::query("UPDATE harness_items SET input_segments=?1 WHERE id=?2")
            .bind(serde_json::to_string(&segments).unwrap())
            .bind(id)
            .execute(boot.repo.pool())
            .await
            .unwrap();
    }
    let (status, body) = replace(&boot, TURN_B, "legacy edit").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn context_usage_and_budget_never_count_as_a_user_message() {
    let boot = two_turns().await;
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id,input_segments FROM harness_items WHERE card_id=?1 AND turn_id=?2 AND input_segments IS NOT NULL")
        .bind(boot.planner_card.id.as_str()).bind(TURN_B).fetch_all(boot.repo.pool()).await.unwrap();
    assert!(!rows.is_empty());
    for (id, raw) in rows {
        let segments: Vec<Value> = serde_json::from_str::<Vec<Value>>(&raw)
            .unwrap()
            .into_iter()
            .filter(|segment| {
                matches!(
                    segment["presentation"].as_str(),
                    Some("system_planner_turn_budget" | "system_planner_context_usage")
                )
            })
            .collect();
        assert_eq!(segments.len(), 2);
        sqlx::query("UPDATE harness_items SET input_segments=?1 WHERE id=?2")
            .bind(serde_json::to_string(&segments).unwrap())
            .bind(id)
            .execute(boot.repo.pool())
            .await
            .unwrap();
    }
    assert_refused_unchanged(&boot, TURN_B, "no message of yours").await;
    boot.harness.shutdown().await.unwrap();
}
