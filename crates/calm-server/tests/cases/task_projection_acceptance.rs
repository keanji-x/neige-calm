//! #985 PR3b acceptance coverage for the document-backed task projection.

#![cfg(unix)]

use std::sync::Arc;

use crate::mcp_wave_report::{Boot, boot as new_boot, call_tool, spec_identity};
use axum::body::Body;
use axum::extract::{FromRef, Path, State};
use axum::http::Request;
use axum::{Extension, Json};
use calm_server::actor::Actor;
use calm_server::auth::Principal;
use calm_server::event::{Event, EventBus};
use calm_server::mcp_server::tools::wave_report::TOOL_REPORT_READ;
use calm_server::mcp_server::tools::wave_report_blocks::{
    TOOL_REPORT_BLOCKS_DELETE, TOOL_REPORT_BLOCKS_UPSERT,
};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes::wave_report_blocks::{
    DeleteReportBlockBody, UpdateReportBlockBody, delete_block, update_block,
};
use calm_server::state::{AppState, CodexClient, DaemonClient, RouteState};
use calm_server::wave_report::tasks_rebuild_tx;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

fn task(key: &str) -> Value {
    json!({
        "key": key, "kind": "codex", "goal": format!("goal {key}"),
        "acceptance": format!("accept {key}"), "context": {"key": key},
        "cwd": format!("/{key}"), "depends_on": [], "priority": 3,
        "gate": {"steps": [{"name": "accept", "cmd": "true"}]},
        "declared_by": "spec", "ready": true
    })
}

async fn read(boot: &Boot) -> Value {
    call_tool(boot, TOOL_REPORT_READ, spec_identity(boot), json!({}))
        .await
        .expect("calm.report.read")
}

async fn upsert(boot: &Boot, id_rev: Option<(&str, u64)>, payload: Value) -> (String, u64) {
    let args = match id_rev {
        Some((id, rev)) => json!({"id": id, "kind": "task", "payload": payload, "if_rev": rev}),
        None => {
            json!({"kind": "task", "payload": payload, "if_doc_rev": read(boot).await["docRev"]})
        }
    };
    let out = call_tool(boot, TOOL_REPORT_BLOCKS_UPSERT, spec_identity(boot), args)
        .await
        .expect("task upsert");
    (
        out["id"].as_str().unwrap().to_string(),
        out["rev"].as_u64().unwrap(),
    )
}

async fn user_upsert(boot: &Boot, id: &str, rev: u64, payload: Value) -> u64 {
    let state = route_state(boot).await;
    update_block(
        State(RouteState::from_ref(&state)),
        principal(),
        Actor("user".into()),
        Path((boot.wave_id.to_string(), id.into())),
        Json(UpdateReportBlockBody {
            kind: "task".into(),
            markdown: None,
            payload: Some(payload),
            if_block_rev: rev as u32,
        }),
    )
    .await
    .expect("user task update")
    .0
    .rev
    .unwrap()
    .into()
}

fn principal() -> Principal {
    Principal {
        user_id: "owner".into(),
        display_name: "owner".into(),
        role: "owner".into(),
        session_id: "test".into(),
    }
}

async fn route_state(boot: &Boot) -> AppState {
    let events = EventBus::new();
    AppState::from_parts(
        boot.repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            boot.repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join(format!(
                "projection-acceptance-plugins-{}",
                uuid::Uuid::new_v4()
            )),
            Vec::new(),
            events,
            boot.ctx.write.clone(),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    )
}

async fn rest_read(boot: &Boot) -> Value {
    let response = calm_server::routes::waves::router()
        .with_state(route_state(boot).await)
        .layer(Extension(principal()))
        .oneshot(
            Request::builder()
                .uri(format!("/api/waves/{}/report", boot.wave_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

async fn assert_diagnosed_on_both_reads(boot: &Boot, key: &str, needle: &str) {
    assert!(
        !keys(boot).await.iter().any(|candidate| candidate == key),
        "{key} row survived"
    );
    assert!(
        diagnostic_contains(&read(boot).await, key, needle),
        "MCP diagnostic {needle}"
    );
    assert!(
        diagnostic_contains(&rest_read(boot).await, key, needle),
        "REST diagnostic {needle}"
    );
}

async fn keys(boot: &Boot) -> Vec<String> {
    sqlx::query_scalar("SELECT key FROM tasks WHERE wave_id=?1 ORDER BY key")
        .bind(boot.wave_id.as_str())
        .fetch_all(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap()
}

fn diagnostic_contains(read: &Value, key: &str, needle: &str) -> bool {
    read["taskDiagnostics"].as_array().unwrap().iter().any(|v| {
        v["key"] == key
            && v["diagnostics"].as_array().unwrap().iter().any(|d| {
                d["message"]
                    .as_str()
                    .is_some_and(|message| message.contains(needle))
            })
    })
}

#[tokio::test]
async fn declare_and_wait_release_and_withdraw_is_end_to_end() {
    let boot = new_boot().await;
    sqlx::query("UPDATE waves SET automation_policy='declare-and-wait' WHERE id=?1")
        .bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let (id, rev) = upsert(&boot, None, task("waited")).await;
    assert!(keys(&boot).await.is_empty());
    assert!(diagnostic_contains(
        &read(&boot).await,
        "waited",
        "requires user release"
    ));

    let mut released = task("waited");
    released["released_by_user"] = json!(true);
    let rev = user_upsert(&boot, &id, rev, released).await;
    assert_eq!(keys(&boot).await, ["waited"]);

    user_upsert(&boot, &id, rev, task("waited")).await;
    assert!(keys(&boot).await.is_empty());

    let mut forbidden = task("forbidden");
    forbidden["released_by_user"] = json!(true);
    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({"kind":"task", "payload":forbidden, "if_doc_rev":read(&boot).await["docRev"]}),
    )
    .await
    .expect_err("spec cannot release");
    assert_eq!(
        err.code,
        calm_server::plugin_host::mcp::RpcError::INVALID_PARAMS
    );
    assert!(err.message.contains("released_by_user"));
}

#[tokio::test]
async fn deleted_tombstone_then_same_key_reproposal_creates_a_fresh_row() {
    let boot = new_boot().await;
    let (id, rev) = upsert(&boot, None, task("phoenix")).await;
    assert_eq!(keys(&boot).await, ["phoenix"]);
    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_DELETE,
        spec_identity(&boot),
        json!({"id":id, "if_rev":rev}),
    )
    .await
    .expect("spec deletion is permitted before user tombstone coverage");

    let (id, rev) = upsert(&boot, None, task("phoenix")).await;

    // User delete normalizes the declaration to a tombstone.
    let state = route_state(&boot).await;
    let _ = delete_block(
        State(RouteState::from_ref(&state)),
        principal(),
        Actor("user".into()),
        Path((boot.wave_id.to_string(), id.clone())),
        Json(DeleteReportBlockBody {
            if_block_rev: rev as u32,
        }),
    )
    .await
    .unwrap();
    assert!(keys(&boot).await.is_empty());

    let block = read(&boot).await["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == id)
        .unwrap()
        .clone();
    let _ = delete_block(
        State(RouteState::from_ref(&state)),
        principal(),
        Actor("user".into()),
        Path((boot.wave_id.to_string(), id)),
        Json(DeleteReportBlockBody {
            if_block_rev: block["rev"].as_u64().unwrap() as u32,
        }),
    )
    .await
    .unwrap();
    upsert(&boot, None, task("phoenix")).await;
    assert_eq!(keys(&boot).await, ["phoenix"]);
}

#[tokio::test]
async fn report_write_emits_three_events_or_two_when_projection_is_unchanged() {
    let boot = new_boot().await;
    let mut rx = boot.ctx.events.subscribe();
    let (id, rev) = upsert(&boot, None, task("events")).await;
    let first: Vec<_> = (0..3).map(|_| ()).collect();
    for _ in first {
        rx.recv().await.unwrap();
    }
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    upsert(&boot, Some((&id, rev)), task("events")).await;
    let events = [rx.recv().await.unwrap(), rx.recv().await.unwrap()];
    assert!(matches!(events[0].event, Event::CardUpdated(_)));
    assert!(matches!(events[1].event, Event::WaveReportEdited { .. }));
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn four_db_diagnostics_delete_rows_and_are_visible_on_mcp_and_rest_reads() {
    // unknown_deps
    let boot = new_boot().await;
    let (id, rev) = upsert(&boot, None, task("unknown")).await;
    let mut bad = task("unknown");
    bad["depends_on"] = json!(["missing"]);
    upsert(&boot, Some((&id, rev)), bad).await;
    assert_diagnosed_on_both_reads(&boot, "unknown", "unknown dependency").await;

    // declare-and-wait
    let boot = new_boot().await;
    let (id, rev) = upsert(&boot, None, task("waiting")).await;
    sqlx::query("UPDATE waves SET automation_policy='declare-and-wait' WHERE id=?1")
        .bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let mut changed = task("waiting");
    changed["goal"] = json!("reevaluate policy");
    upsert(&boot, Some((&id, rev)), changed).await;
    assert_diagnosed_on_both_reads(&boot, "waiting", "requires user release").await;

    // spec_task_ceiling
    let boot = new_boot().await;
    let (id, rev) = upsert(&boot, None, task("ceiling")).await;
    sqlx::query("UPDATE waves SET spec_task_ceiling=0 WHERE id=?1")
        .bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let mut changed = task("ceiling");
    changed["goal"] = json!("reevaluate ceiling");
    upsert(&boot, Some((&id, rev)), changed).await;
    assert_diagnosed_on_both_reads(&boot, "ceiling", "ceiling of 0").await;

    // cross-cove reference
    let boot = new_boot().await;
    let (id, rev) = upsert(&boot, None, task("cross")).await;
    let other_cove = boot
        .repo
        .cove_create(calm_server::model::NewCove {
            name: "other".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let other_wave = boot
        .repo
        .wave_create(calm_server::model::NewWave {
            workflow_input: None,
            cove_id: other_cove.id,
            title: "other".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let mut changed = task("cross");
    changed["refs"] = json!([format!("neige://wave/{}#b_dead", other_wave.id)]);
    upsert(&boot, Some((&id, rev)), changed).await;
    assert_diagnosed_on_both_reads(&boot, "cross", "cross-cove").await;
}

async fn task_bytes(boot: &Boot) -> Vec<String> {
    sqlx::query_scalar("SELECT json_object('key',key,'kind',kind,'goal',goal,'context',context_json,'acceptance',acceptance_criteria,'cwd',cwd,'depends',depends_on_json,'priority',priority,'gate',gate_json,'declared_by',declared_by,'origin',origin,'status',status,'status_detail',status_detail,'worker',worker_card_id,'gate_result',gate_result_json,'gate_pid',gate_pid,'finished',finished_at_ms,'deadline',running_deadline_ms) FROM tasks WHERE wave_id=?1 ORDER BY key")
        .bind(boot.wave_id.as_str()).fetch_all(&boot.repo.sqlite_pool().unwrap()).await.unwrap()
}

#[tokio::test]
async fn rebuild_matches_incremental_bytes_after_adversarial_edit_sequence() {
    let boot = new_boot().await;
    sqlx::query("INSERT INTO tasks(id,wave_id,key,kind,goal,context_json,depends_on_json,priority,status,declared_by,origin,created_at_ms,updated_at_ms) VALUES(?1,?2,'legacy','codex','legacy goal','{}','[]',0,'running','spec','legacy',0,0)")
        .bind(format!("{}:legacy", boot.wave_id)).bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap()).await.unwrap();
    let (a, a_rev) = upsert(&boot, None, task("a")).await;
    let (b, _) = upsert(&boot, None, task("b")).await;
    let (x, x_rev) = upsert(&boot, None, task("x")).await;
    let (y, y_rev) = upsert(&boot, None, task("y")).await;
    let mut duplicate = task("x");
    duplicate["goal"] = json!("duplicate x");
    let (duplicate_id, duplicate_rev) = upsert(&boot, None, duplicate).await;
    assert!(
        !keys(&boot).await.iter().any(|key| key == "x"),
        "duplicate deletes pending rows"
    );
    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_DELETE,
        spec_identity(&boot),
        json!({"id":duplicate_id,"if_rev":duplicate_rev}),
    )
    .await
    .unwrap();
    let mut cycle_x = task("x");
    cycle_x["depends_on"] = json!(["y"]);
    let mut cycle_y = task("y");
    cycle_y["depends_on"] = json!(["x"]);
    upsert(&boot, Some((&x, x_rev)), cycle_x).await;
    upsert(&boot, Some((&y, y_rev)), cycle_y).await;
    assert!(
        !keys(&boot).await.iter().any(|key| key == "x" || key == "y"),
        "cycle deletes rows"
    );
    let mut changed = task("a");
    changed["goal"] = json!("changed goal");
    changed["ready"] = json!(false);
    upsert(&boot, Some((&a, a_rev)), changed).await;
    let before = task_bytes(&boot).await;
    let mut tx = boot.repo.sqlite_pool().unwrap().begin().await.unwrap();
    tasks_rebuild_tx(&mut tx, boot.wave_id.as_str())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        task_bytes(&boot).await,
        before,
        "all declaration and state bytes"
    );
    assert_eq!(keys(&boot).await, vec!["b", "legacy"]);
    assert!(!b.is_empty());
}

#[tokio::test]
async fn inflight_goal_acceptance_and_gate_changes_are_each_detected_without_row_mutation() {
    for field in ["goal", "acceptance", "gate"] {
        let boot = new_boot().await;
        let (id, rev) = upsert(&boot, None, task("flight")).await;
        sqlx::query("UPDATE tasks SET status='running',status_detail='owned' WHERE wave_id=?1 AND key='flight'")
            .bind(boot.wave_id.as_str()).execute(&boot.repo.sqlite_pool().unwrap()).await.unwrap();
        let before = task_bytes(&boot).await;
        let mut changed = task("flight");
        match field {
            "goal" => changed[field] = json!("new goal"),
            "acceptance" => changed[field] = json!("new acceptance"),
            "gate" => changed[field] = json!({"steps":[{"name":"changed","cmd":"false"}]}),
            _ => unreachable!(),
        }
        upsert(&boot, Some((&id, rev)), changed).await;
        assert_eq!(
            task_bytes(&boot).await,
            before,
            "{field} changed frozen task bytes"
        );
        assert!(
            diagnostic_contains(&read(&boot).await, "flight", "declaration changes"),
            "{field}"
        );
    }
}

#[tokio::test]
async fn terminal_spec_key_does_not_consume_ceiling_capacity() {
    let boot = new_boot().await;
    sqlx::query("UPDATE waves SET spec_task_ceiling=1 WHERE id=?1")
        .bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tasks(id,wave_id,key,kind,goal,context_json,depends_on_json,priority,status,declared_by,origin,created_at_ms,updated_at_ms) VALUES(?1,?2,'done','codex','done','{}','[]',0,'done','spec','block',0,0)")
        .bind(format!("{}:done", boot.wave_id)).bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap()).await.unwrap();
    upsert(&boot, None, task("new-capacity")).await;
    assert_eq!(keys(&boot).await, ["done", "new-capacity"]);
    let status: String =
        sqlx::query_scalar("SELECT status FROM tasks WHERE wave_id=?1 AND key='new-capacity'")
            .bind(boot.wave_id.as_str())
            .fetch_one(&boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    assert_eq!(status, "pending");
}
