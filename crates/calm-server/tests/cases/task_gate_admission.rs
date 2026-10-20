//! An agent (MCP) report write that adds or changes a `ready: true` task block carrying a
//! declaration-fixable admission diagnostic (`gate_required`, `gate_cwd_on_agent_task`) is refused
//! whole, in its own transaction. Withdrawn blocks, blocks the write does not touch, runtime
//! waits and user edits are left to the projection.

#![cfg(unix)]

use crate::mcp_track_report::{Boot, boot as new_boot, planner_identity, read_then_commit};
use crate::task_projection_acceptance::{
    has_diagnostic_code, keys, read, task, task_verdict, upsert, user_upsert,
};
use serde_json::{Value, json};

/// The report card row, the projected task rows and the event log: everything a write commits.
async fn committed_state(boot: &Boot) -> (String, Option<Vec<u8>>, Vec<(String, String)>, i64) {
    let pool = boot.repo.sqlite_db().unwrap();
    let (payload, crdt): (String, Option<Vec<u8>>) =
        sqlx::query_as("SELECT payload, body_crdt FROM cards WHERE id=?1")
            .bind(boot.report_card_id.as_str())
            .fetch_one(pool.read_pool())
            .await
            .unwrap();
    let rows = sqlx::query_as("SELECT key, status FROM tasks WHERE track_id=?1 ORDER BY key")
        .bind(boot.track_id.as_str())
        .fetch_all(pool.read_pool())
        .await
        .unwrap();
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(pool.read_pool())
        .await
        .unwrap();
    (payload, crdt, rows, events)
}

fn ungated(key: &str) -> Value {
    let mut payload = task(key);
    payload.as_object_mut().unwrap().remove("gate");
    payload
}

#[tokio::test]
async fn gate_admission_mcp_write_of_an_ungated_ready_task_is_refused_and_writes_nothing() {
    let boot = new_boot().await;
    let (kept_id, _) = upsert(&boot, None, task("kept")).await;
    let before = committed_state(&boot).await;

    // A new block, written beside an unrelated prose edit: the whole commit is refused.
    let error = read_then_commit(
        &boot,
        planner_identity(&boot),
        json!([
            {"op": "upsert", "kind": "prose", "markdown": "# Note\n\nalongside\n"},
            {"op": "upsert", "kind": "task", "payload": ungated("fresh")}
        ]),
    )
    .await
    .expect_err("an ungated ready task is refused");
    assert_eq!(error.code, -32602, "{error:?}");
    for needle in ["`fresh`", "gate_required", "no_gate_reason", "ready: false"] {
        assert!(error.message.contains(needle), "{needle}: {error:?}");
    }
    assert_eq!(committed_state(&boot).await, before, "nothing was written");

    // An existing ready block the write changes is held to the same rule.
    let error = read_then_commit(
        &boot,
        planner_identity(&boot),
        json!([{"op": "upsert", "id": kept_id, "kind": "task", "payload": ungated("kept")}]),
    )
    .await
    .expect_err("removing the gate of a ready task is refused");
    assert_eq!(error.code, -32602, "{error:?}");
    assert!(error.message.contains("`kept`"), "{error:?}");
    assert_eq!(committed_state(&boot).await, before, "nothing was written");
    assert_eq!(keys(&boot).await, vec!["kept".to_string()]);
}

#[tokio::test]
async fn gate_admission_withdrawn_untouched_and_runtime_waiting_blocks_are_not_refused() {
    let boot = new_boot().await;

    // Withdrawn: written, no row.
    let mut withdrawn = ungated("withdrawn");
    withdrawn["ready"] = json!(false);
    let (id, rev) = upsert(&boot, None, withdrawn.clone()).await;
    assert!(keys(&boot).await.is_empty());

    // The user's REST release is written and waits with its diagnostic.
    withdrawn["ready"] = json!(true);
    user_upsert(&boot, &id, rev, withdrawn).await;
    assert!(has_diagnostic_code(
        &read(&boot).await,
        "withdrawn",
        "gate_required"
    ));

    // A later agent write that does not touch that block is not refused by it, and runtime waits
    // (a dependency, the planner ceiling) on the blocks it does write are not refusals either.
    upsert(&boot, None, task("first")).await;
    let mut waiter = task("waiter");
    waiter["depends_on"] = json!(["first"]);
    upsert(&boot, None, waiter).await;
    sqlx::query("UPDATE tracks SET planner_task_ceiling=2 WHERE id=?1")
        .bind(boot.track_id.as_str())
        .execute(boot.repo.sqlite_db().unwrap().writer_pool())
        .await
        .unwrap();
    upsert(&boot, None, task("over-ceiling")).await;

    let snapshot = read(&boot).await;
    assert!(has_diagnostic_code(&snapshot, "withdrawn", "gate_required"));
    assert_eq!(task_verdict(&snapshot, "withdrawn")["schedulable"], false);
    assert!(
        has_diagnostic_code(&snapshot, "over-ceiling", "planner_task_ceiling"),
        "{snapshot}"
    );
    assert_eq!(
        keys(&boot).await,
        vec!["first".to_string(), "waiter".to_string()]
    );
}
