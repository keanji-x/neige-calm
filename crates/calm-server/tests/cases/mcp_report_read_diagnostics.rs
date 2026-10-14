//! Actionable diagnostics are scoped to the text rendered by the real MCP read.
use crate::mcp_track_report::{Boot, assistant_identity, boot, call_tool, planner_identity};
use calm_server::mcp_server::tools::track_report::TOOL_REPORT_READ;
use calm_types::report_blocks::render_fence;
use calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR;
use serde_json::{Value, json};

async fn fixture(duplicate: bool) -> (Boot, String) {
    let boot = boot().await;
    let mut body = "# Overview\n\nA task-free section.\n\n# Tasks\n\n".to_string();
    let pool = boot.repo.sqlite_pool().unwrap();
    for n in 0..24 {
        let key = format!("done-{n}");
        body.push_str(&render_fence("task", &json!({"key":key,"kind":"terminal","command":"true","ready":false,"declared_by":PLANNER_DECLARATION_AUTHOR})));
        body.push('\n');
        sqlx::query(
            "INSERT INTO tasks \
             (id,track_id,key,kind,goal,context_json,depends_on_json,priority, \
              status,declared_by,created_at_ms,updated_at_ms) \
             VALUES(?1,?2,?3,'terminal','true','{}','[]',0,'done',?4,0,0)",
        )
        .bind(format!("task-{n}"))
        .bind(boot.track_id.as_str())
        .bind(key)
        .bind(PLANNER_DECLARATION_AUTHOR)
        .execute(&pool)
        .await
        .unwrap();
    }
    let payload = if duplicate {
        json!({"key":"duplicate","kind":"terminal","command":"true","ready":false,"declared_by":PLANNER_DECLARATION_AUTHOR})
    } else {
        json!({"key":"invalid","kind":"terminal","ready":false,"declared_by":PLANNER_DECLARATION_AUTHOR})
    };
    body.push_str(&render_fence("task", &payload));
    if duplicate {
        body.push('\n');
        body.push_str(&render_fence("task", &payload));
    }
    sqlx::query("UPDATE cards SET body_crdt=NULL,payload=?1 WHERE id=?2")
        .bind(json!({"schemaVersion":4,"summary":"diagnostics","body":body}).to_string())
        .bind(boot.report_card_id.as_str())
        .execute(&pool)
        .await
        .unwrap();
    let blocks = calm_server::track_report_read::load_report_doc_snapshot(
        boot.repo.as_ref(),
        boot.report_card_id.as_str(),
    )
    .await
    .unwrap()
    .blocks;
    let id = blocks
        .iter()
        .find(|b| b.payload["key"] == if duplicate { "duplicate" } else { "invalid" })
        .unwrap()
        .id
        .clone();
    (boot, id)
}

async fn read(boot: &Boot, args: Value) -> Value {
    call_tool(boot, TOOL_REPORT_READ, planner_identity(boot), args)
        .await
        .unwrap()
}

#[tokio::test]
async fn section_excludes_other_owners() {
    let (boot, _) = fixture(false).await;
    let out = read(&boot, json!({"sections":["Overview"]})).await;
    assert_eq!(out["task_diagnostics"], json!([]));
}

#[tokio::test]
async fn block_returns_exactly_its_actionable_item() {
    let (boot, id) = fixture(false).await;
    let out = read(&boot, json!({"blocks":[id]})).await;
    let items = out["task_diagnostics"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{out}");
    assert_eq!(items[0]["block_id"], id);
    assert_eq!(items[0]["diagnostics"][0]["code"], "invalid_declaration");
}

#[tokio::test]
async fn full_excludes_completed_status() {
    let (boot, id) = fixture(false).await;
    let out = read(&boot, json!({})).await;
    let items = out["task_diagnostics"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{out}");
    assert_eq!(items[0]["block_id"], id);
    assert!(
        !out["task_diagnostics"]
            .to_string()
            .contains("task_key_completed")
    );
}

#[tokio::test]
async fn duplicate_evaluates_the_whole_document() {
    let (boot, id) = fixture(true).await;
    let out = read(&boot, json!({"blocks":[id]})).await;
    let items = out["task_diagnostics"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{out}");
    assert_eq!(items[0]["block_id"], id);
    assert!(
        items[0]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "duplicate_key")
    );
}

#[tokio::test]
async fn index_returns_no_diagnostics() {
    let (boot, _) = fixture(false).await;
    assert_eq!(
        read(&boot, json!({"detail":"index"})).await["task_diagnostics"],
        json!([])
    );
}

#[tokio::test]
async fn assistant_receives_no_diagnostics() {
    let (boot, _) = fixture(false).await;
    let out = call_tool(
        &boot,
        TOOL_REPORT_READ,
        assistant_identity(&boot),
        json!({}),
    )
    .await
    .unwrap();
    assert!(out.get("task_diagnostics").is_none());
}

#[tokio::test]
async fn orphan_withdrawals_belong_only_to_full_reads() {
    let (boot, id) = fixture(false).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query(
        "INSERT INTO tasks \
         (id,track_id,key,kind,goal,context_json,depends_on_json,priority, \
          status,declared_by,created_at_ms,updated_at_ms) \
         VALUES('orphan',?1,'orphan','terminal','true','{}','[]',0,'running',?2,0,0)",
    )
    .bind(boot.track_id.as_str())
    .bind(PLANNER_DECLARATION_AUTHOR)
    .execute(&pool)
    .await
    .unwrap();
    let out = read(&boot, json!({})).await;
    let orphan = out["task_diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["key"] == "orphan")
        .expect("full read includes orphan withdrawal");
    assert_eq!(orphan["block_id"], "");
    assert!(
        orphan["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "context_stale_declaration")
    );
    for args in [
        json!({"blocks":[id]}),
        json!({"sections":["Overview"]}),
        json!({"detail":"index"}),
    ] {
        let out = read(&boot, args).await;
        assert!(
            !out["task_diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["key"] == "orphan"),
            "{out}"
        );
    }
}
