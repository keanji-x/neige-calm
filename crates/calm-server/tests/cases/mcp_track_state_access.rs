//! Stored task access through the registered state tool and its renderer.

use super::*;
use calm_server::mcp_server::cli::render::{Render, render};

#[tokio::test]
async fn track_state_projects_stored_access_into_tool_and_text() {
    let boot = boot().await;
    // Same kind and status; neither task has a worker to infer access from.
    insert_task(&boot, "reader-1", "reader", "pending", None).await;
    insert_task(&boot, "writer-1", "writer", "pending", None).await;
    sqlx::query("UPDATE tasks SET access = 'read_only' WHERE id = 'reader-1'")
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let out = call_tool(&boot, TOOL_TRACK_STATE, planner_identity(&boot), json!({}))
        .await
        .unwrap();
    assert_eq!(
        out["tasks"],
        json!([
            {"key":"reader","status":"pending","worker_card_id":null,"access":"read_only","start":"checkout"},
            {"key":"writer","status":"pending","worker_card_id":null,"access":"read_write","start":"checkout"}
        ])
    );
    let text = render(Render::State, TOOL_TRACK_STATE, false, &out).unwrap();
    assert_eq!(
        text,
        format!(
            "track      {}\ntitle      initial\nclosed_at  -\nyou        {} planner\nreport     none\ntasks      reader pending read_only start=checkout\n           writer pending start=checkout\n",
            boot.track_id, boot.planner_card_id
        )
    );
    let json_text = render(Render::State, TOOL_TRACK_STATE, true, &out).unwrap();
    assert_eq!(json_text, format!("{out}\n"));
    assert_eq!(
        serde_json::from_str::<Value>(&json_text).unwrap()["tasks"][0]["access"],
        "read_only"
    );
}
