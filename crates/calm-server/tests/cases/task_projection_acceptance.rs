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
use calm_server::db::sqlite::project_tasks_tx;
use calm_server::event::{EditAuthor, Event, EventBus};
use calm_server::ids::ActorId;
use calm_server::mcp_server::tools::wave_report::TOOL_REPORT_READ;
use calm_server::mcp_server::tools::wave_report_blocks::{
    TOOL_REPORT_BLOCKS_DELETE, TOOL_REPORT_BLOCKS_MOVE, TOOL_REPORT_BLOCKS_UPSERT,
    TOOL_REPORT_WRITE_MARKDOWN,
};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes::wave_report_blocks::{
    DeleteReportBlockBody, UpdateReportBlockBody, delete_block, update_block,
};
use calm_server::state::{AppState, CodexClient, DaemonClient, RouteState};
use calm_server::task_context::{ResolveError, TaskContextMonitor};
use calm_server::wave_report::{WaveReportPayload, persist_report, tasks_rebuild_tx};
use calm_server::wave_report_doc::ReportDoc;
use calm_types::report_blocks::render_fence;
use calm_types::wave_report::ReportBlock;
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

async fn user_upsert(boot: &Boot, id: &str, rev: u64, payload: Value) -> (u64, u64) {
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
    .map(|response| {
        let response = response.0;
        (response.rev.unwrap().into(), response.doc_rev)
    })
    .expect("user task update")
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

async fn rebuild(boot: &Boot) -> calm_server::db::sqlite::TaskProjectionOutcome {
    let mut tx = boot.repo.sqlite_pool().unwrap().begin().await.unwrap();
    let outcome = tasks_rebuild_tx(&mut tx, boot.wave_id.as_str())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    outcome
}

async fn user_delete(boot: &Boot, id: &str, rev: u64) {
    let _ = delete_block(
        State(RouteState::from_ref(&route_state(boot).await)),
        principal(),
        Actor("user".into()),
        Path((boot.wave_id.to_string(), id.into())),
        Json(DeleteReportBlockBody {
            if_block_rev: rev as u32,
        }),
    )
    .await
    .unwrap();
}

async fn patch_policy(boot: &Boot, policy: &str) {
    let response = calm_server::routes::waves::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(route_state(boot).await)
        .oneshot(
            Request::builder()
                .method("PATCH")
                .header("content-type", "application/json")
                .uri(format!("/api/waves/{}", boot.wave_id))
                .body(Body::from(json!({"automation_policy": policy}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
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
    sqlx::query("UPDATE waves SET automation_policy='declare-and-wait',task_budget=0 WHERE id=?1")
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
    let (rev, _) = user_upsert(&boot, &id, rev, released.clone()).await;
    assert_eq!(keys(&boot).await, ["waited"]);

    let (rev, _) = user_upsert(&boot, &id, rev, task("waited")).await;
    assert!(keys(&boot).await.is_empty());

    let (rev, doc_rev_before) = user_upsert(&boot, &id, rev, released).await;
    assert_eq!(keys(&boot).await, ["waited"]);
    sqlx::query("UPDATE tasks SET status='dispatched' WHERE wave_id=?1 AND key='waited'")
        .bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();

    let (_, doc_rev_after) = user_upsert(&boot, &id, rev, task("waited")).await;
    let after_withdraw = read(&boot).await;
    let after_withdraw_rest = rest_read(&boot).await;
    let row = boot
        .repo
        .tasks_by_wave(boot.wave_id.as_str())
        .await
        .unwrap()
        .into_iter()
        .find(|task| task.key == "waited")
        .expect("dispatched row remains");
    assert_eq!(row.status, calm_server::model::TaskStatus::Dispatched);
    assert!(
        row.context_stale_at_ms.is_some(),
        "released_by_user true->false in declare-and-wait must persist a material verdict"
    );
    let advanced: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM events WHERE kind='task.context_advanced' AND scope_wave=?1",
    )
    .bind(boot.wave_id.as_str())
    .fetch_one(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    assert_eq!(advanced, 1, "incremental projection emits exactly once");
    assert!(
        rebuild(&boot).await.kernel_events.is_empty(),
        "an already-stale rebuild emits no duplicate kernel event"
    );
    assert!(doc_rev_after > doc_rev_before);
    for snapshot in [&after_withdraw, &after_withdraw_rest] {
        assert!(diagnostic_contains(
            snapshot,
            "waited",
            "requires user release"
        ));
        assert!(diagnostic_contains(
            snapshot,
            "waited",
            "`waited` is in flight (dispatched) and cannot be withdrawn immediately"
        ));
        assert!(diagnostic_contains(
            snapshot,
            "waited",
            "gate operation that has not started will be rejected"
        ));
    }

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
async fn ready_withdrawal_marks_inflight_material_under_both_policies() {
    for policy in ["auto-declare", "declare-and-wait"] {
        let boot = new_boot().await;
        let mut declaration = task("withdraw-ready");
        let (id, rev) = upsert(&boot, None, declaration.clone()).await;
        sqlx::query("UPDATE tasks SET status='running' WHERE wave_id=?1 AND key='withdraw-ready'")
            .bind(boot.wave_id.as_str())
            .execute(&boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
        patch_policy(&boot, policy).await;
        declaration["ready"] = json!(false);
        user_upsert(&boot, &id, rev, declaration).await;
        let stale: Option<i64> = sqlx::query_scalar(
            "SELECT context_stale_at_ms FROM tasks WHERE wave_id=?1 AND key='withdraw-ready'",
        )
        .bind(boot.wave_id.as_str())
        .fetch_one(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
        assert!(stale.is_some(), "policy {policy}");
    }
}

#[tokio::test]
async fn adopted_legacy_inflight_initializes_ready_shadow_before_withdrawal() {
    let boot = new_boot().await;
    let declaration = task("adopted-ready");
    let (id, rev) = upsert(&boot, None, declaration.clone()).await;
    sqlx::query("UPDATE tasks SET status='running',origin='legacy',decl_ready=0 WHERE wave_id=?1 AND key='adopted-ready'")
        .bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();

    upsert(&boot, Some((&id, rev)), declaration.clone()).await;
    let adopted: (String, i64) = sqlx::query_as(
        "SELECT origin,decl_ready FROM tasks WHERE wave_id=?1 AND key='adopted-ready'",
    )
    .bind(boot.wave_id.as_str())
    .fetch_one(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    assert_eq!(adopted, ("block".into(), 1));

    let latest = read(&boot).await;
    let rev = latest["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["id"] == id)
        .unwrap()["rev"]
        .as_u64()
        .unwrap();
    let before_events = boot.repo.events_since(0, i64::MAX).await.unwrap().len();
    let mut withdrawn = declaration;
    withdrawn["ready"] = json!(false);
    upsert(&boot, Some((&id, rev)), withdrawn).await;

    let stale: Option<i64> = sqlx::query_scalar(
        "SELECT context_stale_at_ms FROM tasks WHERE wave_id=?1 AND key='adopted-ready'",
    )
    .bind(boot.wave_id.as_str())
    .fetch_one(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    assert!(stale.is_some());
    let events = boot.repo.events_since(0, i64::MAX).await.unwrap();
    assert_eq!(
        events[before_events..]
            .iter()
            .filter(|event| matches!(event.3, Event::TaskContextAdvanced { .. }))
            .count(),
        1
    );
    for snapshot in [&read(&boot).await, &rest_read(&boot).await] {
        assert!(diagnostic_contains(
            snapshot,
            "adopted-ready",
            "cannot be withdrawn"
        ));
    }
}

#[tokio::test]
async fn release_edges_obey_current_wait_policy_and_forward_edge_is_safe() {
    let boot = new_boot().await;
    let (id, rev) = upsert(&boot, None, task("release-edge")).await;
    sqlx::query("UPDATE tasks SET status='running' WHERE wave_id=?1 AND key='release-edge'")
        .bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let mut released = task("release-edge");
    released["released_by_user"] = json!(true);
    user_upsert(&boot, &id, rev, released).await;
    let stale: Option<i64> = sqlx::query_scalar(
        "SELECT context_stale_at_ms FROM tasks WHERE wave_id=?1 AND key='release-edge'",
    )
    .bind(boot.wave_id.as_str())
    .fetch_one(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    assert_eq!(stale, None, "false->true is never a withdrawal");

    sqlx::query("UPDATE tasks SET decl_released_by_user=1 WHERE wave_id=?1 AND key='release-edge'")
        .bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();

    let latest = read(&boot).await;
    let rev = latest["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["id"] == id)
        .unwrap()["rev"]
        .as_u64()
        .unwrap();
    let before = boot.repo.events_since(0, i64::MAX).await.unwrap().len();
    user_upsert(&boot, &id, rev, task("release-edge")).await;
    let stale: Option<i64> = sqlx::query_scalar(
        "SELECT context_stale_at_ms FROM tasks WHERE wave_id=?1 AND key='release-edge'",
    )
    .bind(boot.wave_id.as_str())
    .fetch_one(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    assert_eq!(stale, None, "auto-declare ignores user-release withdrawal");
    assert!(
        !boot.repo.events_since(0, i64::MAX).await.unwrap()[before..]
            .iter()
            .any(|event| matches!(event.3, Event::TaskContextAdvanced { .. })),
        "auto-declare release withdrawal must not emit context advancement"
    );
}

#[tokio::test]
async fn same_write_reference_exists_then_target_deletion_removes_pending_projection() {
    let boot = new_boot().await;
    let target = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({"kind": "prose", "markdown": "old target", "if_doc_rev": read(&boot).await["docRev"]}),
    )
    .await
    .unwrap();
    let target_id = target["id"].as_str().unwrap().to_string();
    let target_rev = target["rev"].as_u64().unwrap();
    user_delete(&boot, &target_id, target_rev).await;

    let declaration = {
        let mut value = task("same-write-ref");
        value["refs"] = json!([format!("neige://wave/{}#{target_id}", boot.wave_id)]);
        value
    };
    let before = read(&boot).await;
    let task_id = "b_abcd";
    let blocks = vec![
        ReportBlock {
            id: target_id.clone(),
            kind: "prose".into(),
            rev: 1,
            payload: json!({"markdown": "target text"}),
        },
        ReportBlock {
            id: task_id.into(),
            kind: "task".into(),
            rev: 1,
            payload: declaration.clone(),
        },
    ];
    let body = format!("target text\n\n{}", render_fence("task", &declaration));
    let current = WaveReportPayload {
        schema_version: WaveReportPayload::SCHEMA_VERSION,
        doc_rev: before["docRev"].as_u64().unwrap(),
        summary: String::new(),
        body: body.clone(),
        blocks: Some(blocks),
    };
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query(
        "UPDATE cards SET payload=json(?1),body_crdt=NULL WHERE wave_id=?2 AND kind='wave-report'",
    )
    .bind(serde_json::to_string(&WaveReportPayload::initial()).unwrap())
    .bind(boot.wave_id.as_str())
    .execute(&pool)
    .await
    .unwrap();
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let report = boot
        .repo
        .cards_by_wave(boot.wave_id.as_str())
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.kind == "wave-report")
        .unwrap();
    persist_report(
        boot.repo.as_ref(),
        &boot.ctx.events,
        &boot.ctx.write,
        ActorId::Kernel,
        EditAuthor::Kernel,
        wave,
        report,
        current.clone(),
        current,
        0,
        None,
        None,
        false,
    )
    .await
    .expect("same-write target and referring task");
    let after_write = read(&boot).await;
    assert!(
        keys(&boot).await.contains(&"same-write-ref".into()),
        "same-write projection missing: {after_write}"
    );
    assert!(!diagnostic_contains(
        &read(&boot).await,
        "same-write-ref",
        "does not exist"
    ));

    let snapshot = read(&boot).await;
    let referring_id = task_id;
    let body = format!(
        "<!-- neige:{referring_id} -->\n{}",
        render_fence("task", &declaration)
    );
    call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        spec_identity(&boot),
        json!({"body": body, "if_doc_rev": snapshot["docRev"]}),
    )
    .await
    .expect("delete referenced target");
    assert_diagnosed_on_both_reads(&boot, "same-write-ref", "does not exist").await;
    let error = TaskContextMonitor::new(
        boot.repo.clone(),
        boot.ctx.events.clone(),
        boot.ctx.write.clone(),
    )
    .resolve_task_closure(boot.wave_id.as_str(), "same-write-ref")
    .await
    .expect_err("deleted production block id must be classified");
    assert!(matches!(error, ResolveError::ReferencedBlockAbsent(_)));
}

#[tokio::test]
async fn production_ids_cover_depth_two_referenced_block_absence() {
    let boot = new_boot().await;
    let target = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({"kind":"prose","markdown":"leaf","if_doc_rev":read(&boot).await["docRev"]}),
    )
    .await
    .unwrap();
    let target_id = target["id"].as_str().unwrap().to_string();
    let target_rev = target["rev"].as_u64().unwrap();
    let middle = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({"kind":"prose","markdown":format!("[leaf](neige://wave/{}#{target_id})",boot.wave_id),"if_doc_rev":read(&boot).await["docRev"]}),
    )
    .await
    .unwrap();
    let middle_id = middle["id"].as_str().unwrap().to_string();
    let mut declaration = task("depth-two-absent");
    declaration["refs"] = json!([format!("neige://wave/{}#{middle_id}", boot.wave_id)]);
    upsert(&boot, None, declaration).await;
    user_delete(&boot, &target_id, target_rev).await;
    let error = TaskContextMonitor::new(
        boot.repo.clone(),
        boot.ctx.events.clone(),
        boot.ctx.write.clone(),
    )
    .resolve_task_closure(boot.wave_id.as_str(), "depth-two-absent")
    .await
    .expect_err("deleted depth-two production block id must be classified");
    assert!(matches!(error, ResolveError::ReferencedBlockAbsent(_)));
}

#[tokio::test]
async fn user_declared_release_withdrawal_marks_inflight_even_when_schedulable() {
    let boot = new_boot().await;
    let mut declared = task("user-release-withdrawal");
    let (id, _) = upsert(&boot, None, declared.clone()).await;
    sqlx::query("UPDATE tasks SET status='running',declared_by='user',decl_released_by_user=1 WHERE wave_id=?1 AND key=?2")
        .bind(boot.wave_id.as_str())
        .bind("user-release-withdrawal")
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    declared["released_by_user"] = json!(false);
    declared["declared_by"] = json!("user");
    let (card_id, payload_json, bytes): (String, String, Vec<u8>) = sqlx::query_as(
        "SELECT id,json(payload),body_crdt FROM cards WHERE wave_id=?1 AND kind='wave-report'",
    )
    .bind(boot.wave_id.as_str())
    .fetch_one(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
    doc.upsert_block(Some(&id), "task", &render_fence("task", &declared))
        .unwrap();
    let mut payload: calm_server::wave_report::WaveReportPayload =
        serde_json::from_str(&payload_json).unwrap();
    payload.body = doc.project().unwrap().1;
    payload.blocks = Some(doc.blocks_snapshot().unwrap());
    sqlx::query("UPDATE cards SET payload=json(?1),body_crdt=?2 WHERE id=?3")
        .bind(serde_json::to_string(&payload).unwrap())
        .bind(doc.to_bytes())
        .bind(card_id)
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let before_events = boot.repo.events_since(0, i64::MAX).await.unwrap().len();
    patch_policy(&boot, "declare-and-wait").await;
    let stale: Option<i64> = sqlx::query_scalar(
        "SELECT context_stale_at_ms FROM tasks WHERE wave_id=?1 AND key='user-release-withdrawal'",
    )
    .bind(boot.wave_id.as_str())
    .fetch_one(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    assert!(stale.is_some());
    let persisted_events = boot.repo.events_since(0, i64::MAX).await.unwrap();
    let advanced: Vec<_> = persisted_events[before_events..]
        .iter()
        .filter(|event| matches!(event.3, Event::TaskContextAdvanced { .. }))
        .collect();
    assert_eq!(
        advanced.len(),
        1,
        "PATCH must persist exactly one kernel verdict"
    );
    let actors: Vec<String> = sqlx::query_scalar(
        "SELECT actor FROM events WHERE kind='task.context_advanced' ORDER BY id",
    )
    .fetch_all(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    assert_eq!(actors, [r#"{"kind":"Kernel"}"#]);
    match &advanced[0].3 {
        Event::TaskContextAdvanced {
            changed_refs,
            rationale,
            ..
        } => {
            assert!(changed_refs.is_empty(), "withdrawal is not a hash change");
            assert!(rationale.contains("user release"));
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn terminal_task_does_not_receive_in_flight_withdrawal_diagnostic() {
    let boot = new_boot().await;
    let _ = upsert(&boot, None, task("finished")).await;
    sqlx::query("UPDATE tasks SET status='done' WHERE wave_id=?1 AND key='finished'")
        .bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();

    for snapshot in [&read(&boot).await, &rest_read(&boot).await] {
        assert!(diagnostic_contains(
            snapshot,
            "finished",
            "task key has already completed"
        ));
        assert!(!diagnostic_contains(snapshot, "finished", "in flight"));
    }
}

#[tokio::test]
async fn deleting_in_flight_task_block_keeps_withdrawal_diagnostic_readable() {
    let boot = new_boot().await;
    let (id, rev) = upsert(&boot, None, task("deleted-running")).await;
    sqlx::query("UPDATE tasks SET status='running' WHERE wave_id=?1 AND key='deleted-running'")
        .bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_DELETE,
        spec_identity(&boot),
        json!({"id":id, "if_rev":rev}),
    )
    .await
    .unwrap();

    for snapshot in [&read(&boot).await, &rest_read(&boot).await] {
        assert!(diagnostic_contains(
            snapshot,
            "deleted-running",
            "`deleted-running` is in flight (running)"
        ));
    }
    let stale: Option<i64> = sqlx::query_scalar(
        "SELECT context_stale_at_ms FROM tasks WHERE wave_id=?1 AND key='deleted-running'",
    )
    .bind(boot.wave_id.as_str())
    .fetch_one(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    assert!(
        stale.is_some(),
        "block deletion uses the material primitive"
    );
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
    let events = [
        rx.recv().await.unwrap(),
        rx.recv().await.unwrap(),
        rx.recv().await.unwrap(),
    ];
    assert!(matches!(events[0].event, Event::CardUpdated(_)));
    assert!(matches!(events[1].event, Event::WaveReportEdited { .. }));
    match &events[2].event {
        Event::PlanUpdated { changed_keys, .. } => assert_eq!(changed_keys, &["events"]),
        other => panic!("expected PlanUpdated, got {other:?}"),
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
    sqlx::query_scalar("SELECT json_object('key',key,'kind',kind,'goal',goal,'context',context_json,'acceptance',acceptance_criteria,'cwd',cwd,'depends',depends_on_json,'priority',priority,'gate',gate_json,'declared_by',declared_by,'origin',origin,'decl_ready',decl_ready,'decl_released_by_user',decl_released_by_user,'context_verify_failures',context_verify_failures,'status',status,'status_detail',status_detail,'worker',worker_card_id,'gate_result',gate_result_json,'gate_attempt',gate_attempt,'gate_pid',gate_pid,'gate_pid_starttime',gate_pid_starttime,'gate_pid_boot_id',gate_pid_boot_id,'finished',finished_at_ms,'deadline',running_deadline_ms,'claim_context',claim_context_json,'context_stale',context_stale_at_ms,'closure_truncated',context_closure_truncated) FROM tasks WHERE wave_id=?1 ORDER BY key")
        .bind(boot.wave_id.as_str()).fetch_all(&boot.repo.sqlite_pool().unwrap()).await.unwrap()
}

#[tokio::test]
async fn rebuild_matches_incremental_bytes_after_adversarial_edit_sequence() {
    let boot = new_boot().await;
    sqlx::query("INSERT INTO tasks(id,wave_id,key,kind,goal,context_json,acceptance_criteria,cwd,depends_on_json,priority,gate_json,status,status_detail,declared_by,origin,created_at_ms,updated_at_ms) VALUES(?1,?2,'flight','codex','goal flight','{\"key\":\"flight\"}','accept flight','/flight','[]',3,'{\"steps\":[{\"name\":\"accept\",\"cmd\":\"true\"}]}','running','owned-byte','spec','legacy',0,0)")
        .bind(format!("{}:flight", boot.wave_id)).bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap()).await.unwrap();
    let (a, a_rev) = upsert(&boot, None, task("a")).await;
    let (b, _) = upsert(&boot, None, task("b")).await;
    upsert(&boot, None, task("flight")).await;
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
    // Option (a): damage materialized block rows so rebuild must recreate a
    // surviving pending row and re-adopt the frozen in-flight legacy row.
    sqlx::query("DELETE FROM tasks WHERE wave_id=?1 AND key='b'")
        .bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    sqlx::query("UPDATE tasks SET origin='legacy' WHERE wave_id=?1 AND key='flight'")
        .bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_ne!(task_bytes(&boot).await, before, "damage must be observable");
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
    assert_eq!(keys(&boot).await, vec!["b", "flight"]);
    assert!(!b.is_empty());
}

#[tokio::test]
async fn rebuild_matches_incremental_withdrawal_outcomes_and_exactly_once_events() {
    for released_withdrawal in [false, true] {
        let boot = new_boot().await;
        if released_withdrawal {
            sqlx::query("UPDATE waves SET automation_policy='declare-and-wait' WHERE id=?1")
                .bind(boot.wave_id.as_str())
                .execute(&boot.repo.sqlite_pool().unwrap())
                .await
                .unwrap();
        }
        let mut active = task(if released_withdrawal {
            "released-flight"
        } else {
            "ready-flight"
        });
        let (id, rev) = upsert(&boot, None, active.clone()).await;
        if released_withdrawal {
            active["released_by_user"] = json!(true);
            user_upsert(&boot, &id, rev, active.clone()).await;
        }
        sqlx::query(
            "UPDATE tasks SET status='dispatched',claim_context_json='[]' WHERE wave_id=?1",
        )
        .bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();

        let mut withdrawn = active;
        if released_withdrawal {
            withdrawn["released_by_user"] = json!(false);
        } else {
            withdrawn["ready"] = json!(false);
        }
        let block = ReportBlock {
            id: id.clone(),
            kind: "task".into(),
            rev: 2,
            payload: withdrawn.clone(),
        };
        let (declarations, diagnostics) =
            calm_types::report_blocks::tasks::project_task_declarations(&[block]);
        let mut tx = boot.repo.sqlite_pool().unwrap().begin().await.unwrap();
        let incremental =
            project_tasks_tx(&mut tx, boot.wave_id.as_str(), &declarations, &diagnostics)
                .await
                .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            incremental.kernel_events.len(),
            1,
            "incremental withdrawal event"
        );
        let stale: i64 =
            sqlx::query_scalar("SELECT context_stale_at_ms FROM tasks WHERE wave_id=?1")
                .bind(boot.wave_id.as_str())
                .fetch_one(&boot.repo.sqlite_pool().unwrap())
                .await
                .unwrap();
        let incremental_bytes = task_bytes(&boot).await;

        let (card_id, payload_json, bytes): (String, String, Vec<u8>) = sqlx::query_as(
            "SELECT id,json(payload),body_crdt FROM cards WHERE wave_id=?1 AND kind='wave-report'",
        )
        .bind(boot.wave_id.as_str())
        .fetch_one(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
        let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
        doc.upsert_block(Some(&id), "task", &render_fence("task", &withdrawn))
            .unwrap();
        let mut payload: WaveReportPayload = serde_json::from_str(&payload_json).unwrap();
        payload.body = doc.project().unwrap().1;
        payload.blocks = Some(doc.blocks_snapshot().unwrap());
        sqlx::query("UPDATE cards SET payload=json(?1),body_crdt=?2 WHERE id=?3")
            .bind(serde_json::to_string(&payload).unwrap())
            .bind(doc.to_bytes())
            .bind(card_id)
            .execute(&boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
        sqlx::query("UPDATE tasks SET context_stale_at_ms=NULL WHERE wave_id=?1")
            .bind(boot.wave_id.as_str())
            .execute(&boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
        let rebuilt = rebuild(&boot).await;
        assert_eq!(rebuilt.kernel_events.len(), 1, "rebuild withdrawal event");
        sqlx::query("UPDATE tasks SET context_stale_at_ms=?1 WHERE wave_id=?2")
            .bind(stale)
            .bind(boot.wave_id.as_str())
            .execute(&boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
        assert_eq!(
            task_bytes(&boot).await,
            incremental_bytes,
            "incremental/rebuild task bytes"
        );
        let incremental_payloads: Vec<_> = incremental
            .kernel_events
            .iter()
            .map(|(_, _, event)| serde_json::to_value(event).unwrap())
            .collect();
        let rebuilt_payloads: Vec<_> = rebuilt
            .kernel_events
            .iter()
            .map(|(_, _, event)| serde_json::to_value(event).unwrap())
            .collect();
        assert_eq!(
            rebuilt_payloads, incremental_payloads,
            "kernel event payloads"
        );
        assert!(
            rebuild(&boot).await.kernel_events.is_empty(),
            "stale rebuild is event-idempotent"
        );
    }
}

#[tokio::test]
async fn inflight_goal_acceptance_and_gate_changes_are_each_detected_without_row_mutation() {
    for field in ["goal", "acceptance", "gate", "context", "cwd", "depends_on"] {
        let boot = new_boot().await;
        let (id, rev) = upsert(&boot, None, task("flight")).await;
        sqlx::query("UPDATE tasks SET status='running',status_detail='owned' WHERE wave_id=?1 AND key='flight'")
            .bind(boot.wave_id.as_str()).execute(&boot.repo.sqlite_pool().unwrap()).await.unwrap();
        let before = task_bytes(&boot).await;
        upsert(&boot, Some((&id, rev)), task("flight")).await;
        assert_eq!(
            task_bytes(&boot).await,
            before,
            "unchanged declaration bytes"
        );
        assert!(
            !diagnostic_contains(&read(&boot).await, "flight", "declaration changes"),
            "unchanged declaration must not be stale"
        );
        let rev = read(&boot).await["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|block| block["id"] == id)
            .unwrap()["rev"]
            .as_u64()
            .unwrap();
        let mut changed = task("flight");
        match field {
            "goal" => changed[field] = json!("new goal"),
            "acceptance" => changed[field] = json!("new acceptance"),
            "gate" => changed[field] = json!({"steps":[{"name":"changed","cmd":"false"}]}),
            "context" => changed[field] = json!({"changed": true}),
            "cwd" => changed[field] = json!("/changed"),
            "depends_on" => changed[field] = json!(["other"]),
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
async fn inflight_priority_and_declared_by_changes_do_not_promise_context_rejection() {
    for (field, value) in [("priority", json!(99)), ("declared_by", json!("user"))] {
        let boot = new_boot().await;
        let (id, rev) = upsert(&boot, None, task("non-context-drift")).await;
        sqlx::query(
            "UPDATE tasks SET status='running' WHERE wave_id=?1 AND key='non-context-drift'",
        )
        .bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
        if field == "declared_by" {
            sqlx::query(
                "UPDATE tasks SET declared_by='user' WHERE wave_id=?1 AND key='non-context-drift'",
            )
            .bind(boot.wave_id.as_str())
            .execute(&boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
            rebuild(&boot).await;
        } else {
            let mut changed = task("non-context-drift");
            changed[field] = value;
            upsert(&boot, Some((&id, rev)), changed).await;
        }
        let stale: Option<i64> = sqlx::query_scalar(
            "SELECT context_stale_at_ms FROM tasks WHERE wave_id=?1 AND key='non-context-drift'",
        )
        .bind(boot.wave_id.as_str())
        .fetch_one(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
        assert_eq!(stale, None, "{field}");
        for snapshot in [&read(&boot).await, &rest_read(&boot).await] {
            assert!(
                !diagnostic_contains(snapshot, "non-context-drift", "gate operation"),
                "{field} must not promise a stale-context rejection"
            );
        }
    }
}

#[tokio::test]
async fn legacy_canonical_gate_and_context_are_semantically_equal_to_block_declaration() {
    let boot = new_boot().await;
    let (id, rev) = upsert(&boot, None, task("flight")).await;
    sqlx::query("UPDATE tasks SET status='running',origin='legacy',gate_json=?1,context_json=?2 WHERE wave_id=?3 AND key='flight'")
        .bind(r#"{"steps":[{"name":"accept","cmd":"true"}]}"#)
        .bind(r#"{"key":"flight"}"#)
        .bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap()).await.unwrap();
    upsert(&boot, Some((&id, rev)), task("flight")).await;
    assert!(
        !diagnostic_contains(&read(&boot).await, "flight", "declaration changes"),
        "legacy JSON spelling must not create a stale diagnostic"
    );
}

#[tokio::test]
async fn unknown_dependencies_use_all_inflight_rows_but_not_legacy_pending_rows() {
    let boot = new_boot().await;
    sqlx::query("INSERT INTO tasks(id,wave_id,key,kind,goal,context_json,depends_on_json,priority,status,declared_by,origin,created_at_ms,updated_at_ms) VALUES(?1,?2,'old-running','codex','old','{}','[]',0,'running','spec','legacy',0,0), (?3,?2,'old-pending','codex','old','{}','[]',0,'pending','spec','legacy',0,0)")
        .bind(format!("{}:old-running", boot.wave_id)).bind(boot.wave_id.as_str())
        .bind(format!("{}:old-pending", boot.wave_id))
        .execute(&boot.repo.sqlite_pool().unwrap()).await.unwrap();
    let mut known = task("uses-running");
    known["depends_on"] = json!(["old-running"]);
    upsert(&boot, None, known).await;
    assert!(keys(&boot).await.contains(&"uses-running".into()));

    let mut unknown = task("uses-pending");
    unknown["depends_on"] = json!(["old-pending"]);
    upsert(&boot, None, unknown).await;
    assert_diagnosed_on_both_reads(&boot, "uses-pending", "unknown dependency `old-pending`").await;
}

#[tokio::test]
async fn deleting_dependency_converges_in_one_evaluation_and_rebuild_matches_reads() {
    let boot = new_boot().await;
    let (k1, k1_rev) = upsert(&boot, None, task("k1")).await;
    let mut k2 = task("k2");
    k2["depends_on"] = json!(["k1"]);
    upsert(&boot, None, k2).await;
    assert_eq!(keys(&boot).await, ["k1", "k2"]);
    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_DELETE,
        spec_identity(&boot),
        json!({"id":k1,"if_rev":k1_rev}),
    )
    .await
    .unwrap();
    assert_diagnosed_on_both_reads(&boot, "k2", "unknown dependency `k1`").await;
    let outcome = rebuild(&boot).await;
    assert!(outcome.diagnostics.iter().any(|v| {
        v.key == "k2"
            && v.diagnostics
                .iter()
                .any(|d| d.message.contains("unknown dependency `k1`"))
    }));
    assert!(keys(&boot).await.is_empty());
}

#[tokio::test]
async fn uncleared_user_tombstone_derives_wait_and_explicit_auto_declare_recovers() {
    let boot = new_boot().await;
    let (denied, denied_rev) = upsert(&boot, None, task("denied")).await;
    user_delete(&boot, &denied, denied_rev).await;
    let (replacement, _) = upsert(&boot, None, task("replacement")).await;
    assert_diagnosed_on_both_reads(&boot, "replacement", "requires user release").await;
    assert!(
        rest_read(&boot).await["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["id"] == denied && b["payload"]["tombstoned_by"] == "user")
    );
    patch_policy(&boot, "auto-declare").await;
    assert_eq!(keys(&boot).await, ["replacement"]);
    assert!(
        rest_read(&boot).await["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["id"] == denied && b["payload"]["tombstoned_by"] == "user")
    );
    assert!(!replacement.is_empty());
}

#[tokio::test]
async fn user_veto_removes_other_unreleased_pending_rows_with_readable_reason() {
    let boot = new_boot().await;
    let (vetoed, vetoed_rev) = upsert(&boot, None, task("vetoed")).await;
    upsert(&boot, None, task("collateral")).await;
    assert_eq!(keys(&boot).await, ["collateral", "vetoed"]);
    user_delete(&boot, &vetoed, vetoed_rev).await;
    assert_diagnosed_on_both_reads(&boot, "collateral", "requires user release").await;
}

#[tokio::test]
async fn invalid_task_payload_deletes_pending_row_with_readable_reason() {
    let boot = new_boot().await;
    let (id, rev) = upsert(&boot, None, task("invalidated")).await;
    let mut invalid = task("invalidated");
    invalid["acceptance"] = json!("");
    let (card_id, payload_json, bytes): (String, String, Vec<u8>) = sqlx::query_as(
        "SELECT id,json(payload),body_crdt FROM cards WHERE wave_id=?1 AND kind='wave-report'",
    )
    .bind(boot.wave_id.as_str())
    .fetch_one(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
    doc.upsert_block(Some(&id), "task", &render_fence("task", &invalid))
        .unwrap();
    let mut payload: calm_server::wave_report::WaveReportPayload =
        serde_json::from_str(&payload_json).unwrap();
    payload.body = doc.project().unwrap().1;
    payload.blocks = Some(doc.blocks_snapshot().unwrap());
    sqlx::query("UPDATE cards SET payload=json(?1),body_crdt=?2 WHERE id=?3")
        .bind(serde_json::to_string(&payload).unwrap())
        .bind(doc.to_bytes())
        .bind(card_id)
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let _ = rev;
    rebuild(&boot).await;
    assert_diagnosed_on_both_reads(&boot, "invalidated", "acceptance").await;
}

#[tokio::test]
async fn malformed_gate_keeps_keyed_declaration_and_readable_payload_diagnostic() {
    let boot = new_boot().await;
    let (id, _) = upsert(&boot, None, task("invalid-gate")).await;
    let (card_id, payload_json, bytes): (String, String, Vec<u8>) = sqlx::query_as(
        "SELECT id,json(payload),body_crdt FROM cards WHERE wave_id=?1 AND kind='wave-report'",
    )
    .bind(boot.wave_id.as_str())
    .fetch_one(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    let mut invalid = task("invalid-gate");
    invalid["gate"] = Value::Null;
    let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
    doc.upsert_block(Some(&id), "task", &render_fence("task", &invalid))
        .unwrap();
    let mut payload: calm_server::wave_report::WaveReportPayload =
        serde_json::from_str(&payload_json).unwrap();
    payload.body = doc.project().unwrap().1;
    payload.blocks = Some(doc.blocks_snapshot().unwrap());
    sqlx::query("UPDATE cards SET payload=json(?1),body_crdt=?2 WHERE id=?3")
        .bind(serde_json::to_string(&payload).unwrap())
        .bind(doc.to_bytes())
        .bind(card_id)
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    rebuild(&boot).await;
    assert!(!keys(&boot).await.contains(&"invalid-gate".into()));
    assert_diagnosed_on_both_reads(&boot, "invalid-gate", "gate").await;
}

#[tokio::test]
async fn ceiling_rebuild_is_stable_and_only_new_candidate_is_rejected() {
    let boot = new_boot().await;
    sqlx::query("UPDATE waves SET spec_task_ceiling=2 WHERE id=?1")
        .bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    upsert(&boot, None, task("k1")).await;
    upsert(&boot, None, task("k2")).await;
    let settled = task_bytes(&boot).await;
    upsert(&boot, None, task("k3")).await;
    assert_eq!(
        task_bytes(&boot).await,
        settled,
        "k1/k2 bytes must not move"
    );
    assert_diagnosed_on_both_reads(&boot, "k3", "ceiling of 2").await;
    rebuild(&boot).await;
    let once = task_bytes(&boot).await;
    rebuild(&boot).await;
    assert_eq!(
        task_bytes(&boot).await,
        once,
        "two rebuilds must be byte-identical"
    );
}

#[tokio::test]
async fn document_order_is_ceiling_priority_and_move_reprojects_pending_rows() {
    let boot = new_boot().await;
    sqlx::query("UPDATE waves SET spec_task_ceiling=1 WHERE id=?1")
        .bind(boot.wave_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    upsert(&boot, None, task("first")).await;
    let (second, _) = upsert(&boot, None, task("second")).await;
    assert_eq!(keys(&boot).await, ["first"]);
    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_MOVE,
        spec_identity(&boot),
        json!({"id":second,"to_index":0,"if_doc_rev":read(&boot).await["docRev"]}),
    )
    .await
    .unwrap();
    assert_eq!(keys(&boot).await, ["second"]);
    assert_diagnosed_on_both_reads(&boot, "first", "ceiling of 1").await;
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
