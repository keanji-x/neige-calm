//! #985 PR3b acceptance coverage for the document-backed task projection.

#![cfg(unix)]

use std::future::Future;
use std::sync::Arc;
use std::task::Poll;

use crate::mcp_track_report::{Boot, boot as new_boot, call_tool, planner_identity};
use axum::body::Body;
use axum::extract::{FromRef, Path, State};
use axum::http::Request;
use axum::{Extension, Json};
use calm_server::actor::Actor;
use calm_server::auth::Principal;
use calm_server::db::sqlite::{begin_immediate_tx, project_tasks_tx, task_claim_pending_tx};
use calm_server::event::{EditAuthor, Event, EventBus};
use calm_server::ids::ActorId;
use calm_server::mcp_server::tools::track_report::TOOL_REPORT_READ;
use calm_server::mcp_server::tools::track_report_blocks::{
    TOOL_REPORT_BLOCKS_DELETE, TOOL_REPORT_BLOCKS_MOVE, TOOL_REPORT_BLOCKS_UPSERT,
    TOOL_REPORT_WRITE_MARKDOWN,
};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes::track_report_blocks::{
    DeleteReportBlockBody, UpdateReportBlockBody, delete_block, update_block,
};
use calm_server::state::{AppState, CodexClient, DaemonClient, RouteState};
use calm_server::task_context::{ResolveError, TaskContextMonitor};
use calm_server::track_report::{TrackReportPayload, persist_report, tasks_rebuild_tx};
use calm_server::track_report_doc::ReportDoc;
use calm_types::event::TaskContextRef;
use calm_types::report_blocks::render_fence;
use calm_types::track_report::ReportBlock;
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
    call_tool(boot, TOOL_REPORT_READ, planner_identity(boot), json!({}))
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
    let out = call_tool(
        boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        planner_identity(boot),
        args,
    )
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
        Path((boot.track_id.to_string(), id.into())),
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
    let state = AppState::from_parts(
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
    );
    state.dispatcher.abort_event_listener_for_test();
    state
}

async fn rest_read(boot: &Boot) -> Value {
    let response = calm_server::routes::tracks::router()
        .with_state(route_state(boot).await)
        .layer(Extension(principal()))
        .oneshot(
            Request::builder()
                .uri(format!("/api/tracks/{}/report", boot.track_id))
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
    let pool = boot.repo.sqlite_pool().unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    let outcome = tasks_rebuild_tx(&mut tx, boot.track_id.as_str())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    outcome
}

/// #1070 regression for the pre-#1080 deferred rebuild helper. The rebuild
/// transaction owns the writer slot before it reads projection inputs, so a
/// distinct writer waits without forming a shared-cache lock cycle.
#[tokio::test]
async fn immediate_rebuild_serializes_with_waiting_writer_without_deadlock() {
    let boot = new_boot().await;
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,depends_on_json,priority,status,declared_by,created_at_ms,updated_at_ms) VALUES(?1,?2,'repro-damage','codex','damage','{}','[]',0,'pending','spec',0,0)",
    )
    .bind(format!("{}:repro-damage", boot.track_id))
    .bind(boot.track_id.as_str())
    .execute(&pool)
    .await
    .unwrap();

    let mut rebuild_tx = begin_immediate_tx(&pool).await.unwrap();
    let _: i64 = sqlx::query_scalar("SELECT count(*) FROM cards WHERE track_id=?1")
        .bind(boot.track_id.as_str())
        .fetch_one(&mut *rebuild_tx)
        .await
        .unwrap();

    let mut waiting_writer = Box::pin(begin_immediate_tx(&pool));
    std::future::poll_fn(|cx| match waiting_writer.as_mut().poll(cx) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(_) => panic!("rebuild transaction must already own the writer slot"),
    })
    .await;

    tasks_rebuild_tx(&mut rebuild_tx, boot.track_id.as_str())
        .await
        .unwrap();
    rebuild_tx.commit().await.unwrap();

    let mut writer_tx = waiting_writer.await.unwrap();
    sqlx::query("UPDATE cards SET updated_at=updated_at WHERE id=?1")
        .bind(boot.report_card_id.as_str())
        .execute(&mut *writer_tx)
        .await
        .unwrap();
    writer_tx.commit().await.unwrap();
    assert!(!keys(&boot).await.contains(&"repro-damage".to_string()));
}

async fn user_delete(boot: &Boot, id: &str, rev: u64) {
    let _ = delete_block(
        State(RouteState::from_ref(&route_state(boot).await)),
        principal(),
        Actor("user".into()),
        Path((boot.track_id.to_string(), id.into())),
        Json(DeleteReportBlockBody {
            if_block_rev: rev as u32,
        }),
    )
    .await
    .unwrap();
}

async fn patch_policy(boot: &Boot, policy: &str) {
    let response = calm_server::routes::tracks::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(route_state(boot).await)
        .oneshot(
            Request::builder()
                .method("PATCH")
                .header("content-type", "application/json")
                .uri(format!("/api/tracks/{}", boot.track_id))
                .body(Body::from(json!({"automation_policy": policy}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
}

async fn keys(boot: &Boot) -> Vec<String> {
    sqlx::query_scalar("SELECT key FROM tasks WHERE track_id=?1 ORDER BY key")
        .bind(boot.track_id.as_str())
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

fn has_diagnostic_code(read: &Value, key: &str, code: &str) -> bool {
    read["taskDiagnostics"].as_array().unwrap().iter().any(|v| {
        v["key"] == key
            && v["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|diagnostic| diagnostic["code"] == code)
    })
}

fn task_verdict<'a>(read: &'a Value, key: &str) -> &'a Value {
    read["taskDiagnostics"]
        .as_array()
        .expect("taskDiagnostics array")
        .iter()
        .find(|verdict| verdict["key"] == key)
        .unwrap_or_else(|| panic!("task verdict for {key}"))
}

/// #1260 — one read must distinguish the three states that used to collapse
/// into a bare `pending`/blank row. The fixture is one real projection:
///
/// * `dependency-blocked` has a row but waits for running `occupier`;
/// * `budget-queued` has a row and ready dependencies, but the explicit 1-slot
///   task budget is occupied;
/// * `not-admitted` is the fourth ready declaration under a planner ceiling of
///   three, so it deliberately has no `tasks` row.
///
/// Both public reads are asserted because the FE consumes REST while planners
/// consume MCP; they must not tell two stories about the same track.
#[tokio::test]
async fn pending_reasons_distinguish_dependency_budget_and_admission() {
    let boot = new_boot().await;
    sqlx::query("UPDATE tracks SET planner_task_ceiling=3,task_budget=1 WHERE id=?1")
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();

    upsert(&boot, None, task("occupier")).await;
    let mut dependency_blocked = task("dependency-blocked");
    dependency_blocked["depends_on"] = json!(["occupier"]);
    upsert(&boot, None, dependency_blocked).await;
    upsert(&boot, None, task("budget-queued")).await;
    upsert(&boot, None, task("not-admitted")).await;

    sqlx::query("UPDATE tasks SET status='running' WHERE track_id=?1 AND key='occupier'")
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();

    for (surface, response) in [("MCP", read(&boot).await), ("REST", rest_read(&boot).await)] {
        let dependency = &task_verdict(&response, "dependency-blocked")["pendingReason"];
        assert_eq!(dependency["kind"], "dependencyBlocked", "{surface}");
        assert_eq!(dependency["dependencies"], json!(["occupier"]), "{surface}");
        assert!(
            dependency["message"]
                .as_str()
                .is_some_and(|message| message.contains("occupier")),
            "{surface}: {dependency}"
        );

        let queued = &task_verdict(&response, "budget-queued")["pendingReason"];
        assert_eq!(queued["kind"], "budgetQueued", "{surface}");
        assert_eq!(queued["occupiedTaskBudget"], 1, "{surface}");
        assert_eq!(queued["effectiveTaskBudget"], 1, "{surface}");
        assert_eq!(queued["message"], "Queued 1/1");

        let rejected = &task_verdict(&response, "not-admitted")["pendingReason"];
        assert_eq!(rejected["kind"], "notAdmitted", "{surface}");
        assert!(
            rejected["diagnosticCodes"]
                .as_array()
                .is_some_and(|codes| codes.iter().any(|code| code == "planner_task_ceiling")),
            "{surface}: {rejected}"
        );
        assert!(
            rejected["message"]
                .as_str()
                .is_some_and(|message| message.starts_with("Not admitted")),
            "{surface}: {rejected}"
        );
    }

    sqlx::query("UPDATE tracks SET task_budget=NULL WHERE id=?1")
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let with_default = calm_server::track_report_read::load_report_read_snapshot(
        boot.repo.as_ref(),
        boot.report_card_id.as_str(),
        1,
    )
    .await
    .unwrap();
    let default_reason = with_default
        .task_diagnostics
        .iter()
        .find(|verdict| verdict.key == "budget-queued")
        .and_then(|verdict| verdict.pending_reason.as_ref())
        .expect("server default produces a budget reason");
    assert!(matches!(
        default_reason,
        calm_server::db::sqlite::TaskPendingReason::BudgetQueued {
            occupied_task_budget: 1,
            effective_task_budget: 1,
            ..
        }
    ));

    let with_room = calm_server::track_report_read::load_report_read_snapshot(
        boot.repo.as_ref(),
        boot.report_card_id.as_str(),
        2,
    )
    .await
    .unwrap();
    assert!(
        with_room
            .task_diagnostics
            .iter()
            .find(|verdict| verdict.key == "budget-queued")
            .is_some_and(|verdict| verdict.pending_reason.is_none()),
        "an environment default with a free slot must not diagnose budget queueing"
    );

    sqlx::query("UPDATE tracks SET task_budget=1 WHERE id=?1")
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let override_wins = calm_server::track_report_read::load_report_read_snapshot(
        boot.repo.as_ref(),
        boot.report_card_id.as_str(),
        9,
    )
    .await
    .unwrap();
    assert!(override_wins.task_diagnostics.iter().any(|verdict| {
        verdict.key == "budget-queued"
            && matches!(
                verdict.pending_reason.as_ref(),
                Some(calm_server::db::sqlite::TaskPendingReason::BudgetQueued {
                    effective_task_budget: 1,
                    ..
                })
            )
    }));
}

#[tokio::test]
async fn malformed_persisted_dependency_shape_keeps_report_reads_available() {
    let boot = new_boot().await;
    upsert(&boot, None, task("legacy-shape")).await;
    sqlx::query("UPDATE tasks SET depends_on_json='{}' WHERE track_id=?1 AND key='legacy-shape'")
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();

    for (surface, response) in [("MCP", read(&boot).await), ("REST", rest_read(&boot).await)] {
        let verdict = task_verdict(&response, "legacy-shape");
        assert_eq!(verdict["status"], "pending", "{surface}: {verdict}");
        assert!(verdict["pendingReason"].is_null(), "{surface}: {verdict}");
    }
}

#[tokio::test]
async fn syntactically_invalid_persisted_dependencies_also_degrade_to_empty() {
    let boot = new_boot().await;
    upsert(&boot, None, task("corrupt-dependencies")).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    let mut connection = pool.acquire().await.unwrap();
    sqlx::query("PRAGMA ignore_check_constraints=ON")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE tasks SET depends_on_json='not-json' WHERE track_id=?1 AND key='corrupt-dependencies'",
    )
    .bind(boot.track_id.as_str())
    .execute(&mut *connection)
    .await
    .unwrap();
    sqlx::query("PRAGMA ignore_check_constraints=OFF")
        .execute(&mut *connection)
        .await
        .unwrap();
    drop(connection);

    for (surface, response) in [("MCP", read(&boot).await), ("REST", rest_read(&boot).await)] {
        let verdict = task_verdict(&response, "corrupt-dependencies");
        assert_eq!(verdict["status"], "pending", "{surface}: {verdict}");
        assert!(verdict["pendingReason"].is_null(), "{surface}: {verdict}");
    }
}

#[tokio::test]
async fn fresh_reference_error_overrides_an_existing_pending_rows_old_queue_reason() {
    let boot = new_boot().await;
    let source = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let target = boot
        .repo
        .track_create(calm_server::model::NewTrack {
            template_input: None,
            area_id: source.area_id,
            title: "target".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let target_block = ReportBlock {
        id: "b_dead".into(),
        kind: "prose".into(),
        rev: 1,
        payload: json!({"markdown": "target"}),
    };
    let target_report = boot
        .repo
        .card_create(calm_server::model::NewCard {
            track_id: target.id.clone(),
            kind: "track-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
            title: None,
        })
        .await
        .unwrap();
    sqlx::query(
        "UPDATE cards SET payload=json_set(payload,'$.body','target','$.blocks',json(?1)) WHERE id=?2",
    )
    .bind(serde_json::to_string(&vec![target_block]).unwrap())
    .bind(target_report.id.as_str())
    .execute(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    let mut declaration = task("source-task");
    declaration["refs"] = json!([format!("neige://wave/{}#b_dead", target.id)]);
    upsert(&boot, None, declaration).await;
    assert!(keys(&boot).await.iter().any(|key| key == "source-task"));

    sqlx::query("UPDATE cards SET payload=json_set(payload,'$.blocks',json('[]')) WHERE id=?1")
        .bind(target_report.id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();

    for (surface, response) in [("MCP", read(&boot).await), ("REST", rest_read(&boot).await)] {
        let verdict = task_verdict(&response, "source-task");
        assert_eq!(verdict["status"], "pending", "{surface}: {verdict}");
        assert_eq!(
            verdict["pendingReason"]["kind"], "notAdmitted",
            "{surface}: {verdict}"
        );
        assert!(
            verdict["pendingReason"]["diagnosticCodes"]
                .as_array()
                .is_some_and(|codes| codes.iter().any(|code| code == "reference_missing")),
            "{surface}: {verdict}"
        );
    }
}

#[tokio::test]
async fn terminal_dependency_reason_tells_the_planner_to_revise_the_plan() {
    let boot = new_boot().await;
    upsert(&boot, None, task("failed-first")).await;
    let mut blocked = task("blocked-next");
    blocked["depends_on"] = json!(["failed-first", "failed-first"]);
    upsert(&boot, None, blocked).await;
    sqlx::query("UPDATE tasks SET status='failed' WHERE track_id=?1 AND key='failed-first'")
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();

    for (surface, response) in [("MCP", read(&boot).await), ("REST", rest_read(&boot).await)] {
        let reason = &task_verdict(&response, "blocked-next")["pendingReason"];
        assert_eq!(reason["kind"], "dependencyBlocked", "{surface}");
        assert_eq!(reason["dependencies"], json!(["failed-first"]), "{surface}");
        let message = reason["message"].as_str().unwrap();
        assert!(message.contains("failed"), "{surface}: {message}");
        assert!(
            message.contains("revise dependencies"),
            "{surface}: {message}"
        );
        assert!(!message.starts_with("Waiting"), "{surface}: {message}");
    }
}

#[tokio::test]
async fn report_blocks_gate_admission_matrix_pins_diagnostics_and_projection() {
    for require_gates in [false, true] {
        let boot = new_boot().await;
        boot.repo
            .track_update(
                boot.track_id.as_str(),
                calm_server::model::TrackPatch {
                    require_task_gates: Some(require_gates),
                    ..Default::default()
                },
            )
            .await
            .expect("set gate policy");

        let mut cases = Vec::new();
        for kind in ["codex", "claude"] {
            cases.push((
                format!("{kind}-gated"),
                kind,
                Some(json!({"steps": [{"name": "check", "cmd": "true"}]})),
                None,
                true,
            ));
            cases.push((
                format!("{kind}-reason"),
                kind,
                None,
                Some("verified externally"),
                true,
            ));
            cases.push((format!("{kind}-missing"), kind, None, None, !require_gates));
        }
        cases.push(("terminal-missing".into(), "terminal", None, None, true));

        for (key, kind, gate, no_gate_reason, should_project) in cases {
            let mut payload = json!({
                "key": key,
                "kind": kind,
                "ready": true,
                "declared_by": "spec"
            });
            let instruction_field = if kind == "terminal" {
                "command"
            } else {
                "goal"
            };
            payload[instruction_field] = json!(format!("exercise {key}"));
            if let Some(gate) = gate {
                payload["gate"] = gate;
            }
            if let Some(reason) = no_gate_reason {
                payload["no_gate_reason"] = json!(reason);
            }

            upsert(&boot, None, payload).await;
            let snapshot = read(&boot).await;
            let row_exists = keys(&boot).await.iter().any(|candidate| candidate == &key);
            let gate_required = has_diagnostic_code(&snapshot, &key, "gate_required");

            assert_eq!(
                row_exists, should_project,
                "require_gates={require_gates}, kind={kind}, key={key}: projection"
            );
            assert_eq!(
                gate_required, !should_project,
                "require_gates={require_gates}, kind={kind}, key={key}: diagnostics"
            );
        }
    }
}

#[tokio::test]
async fn production_reads_attach_task_state_and_read_time_diagnostics() {
    let boot = new_boot().await;
    let (block_id, _) = upsert(&boot, None, task("read-boundary")).await;
    let task_id: String =
        sqlx::query_scalar("SELECT id FROM tasks WHERE track_id=?1 AND key='read-boundary'")
            .bind(boot.track_id.as_str())
            .fetch_one(&boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    let context = [TaskContextRef {
        track_id: boot.track_id.clone(),
        block_id,
        rev: 1,
        hash: "frozen".into(),
        is_root: false,
    }];
    let mut tx = boot.repo.sqlite_pool().unwrap().begin().await.unwrap();
    assert_eq!(
        task_claim_pending_tx(&mut tx, &task_id, 42, &context, true)
            .await
            .unwrap(),
        1
    );
    tx.commit().await.unwrap();
    sqlx::query("UPDATE tasks SET gate_result_json=?1 WHERE id=?2")
        .bind(
            json!({
                "passed": false,
                "failing_step": "tests",
                "log_path": "/private/server/gate.log",
                "log_tail": "secret implementation output",
                "attempt": 1
            })
            .to_string(),
        )
        .bind(&task_id)
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();

    for (name, response) in [("MCP", read(&boot).await), ("REST", rest_read(&boot).await)] {
        let verdict = response["taskDiagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|verdict| verdict["key"] == "read-boundary")
            .unwrap_or_else(|| panic!("{name} verdict"));
        assert_eq!(verdict["status"], "dispatched", "{name} projected status");
        let gate_result = verdict["gateResult"]
            .as_object()
            .expect("projected gate result");
        assert!(
            !gate_result.contains_key("log_path"),
            "{name} hides gate log path"
        );
        assert!(
            !gate_result.contains_key("log_tail"),
            "{name} hides gate log tail"
        );
        assert!(
            verdict["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|diagnostic| diagnostic["code"] == "reference_chain_too_large"),
            "{name} read-time diagnostic"
        );
    }
}

#[tokio::test]
async fn declare_and_wait_release_and_withdraw_is_end_to_end() {
    let boot = new_boot().await;
    sqlx::query("UPDATE tracks SET automation_policy='declare-and-wait',task_budget=0 WHERE id=?1")
        .bind(boot.track_id.as_str())
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
    sqlx::query("UPDATE tasks SET status='dispatched' WHERE track_id=?1 AND key='waited'")
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();

    let (_, doc_rev_after) = user_upsert(&boot, &id, rev, task("waited")).await;
    let after_withdraw = read(&boot).await;
    let after_withdraw_rest = rest_read(&boot).await;
    let row = boot
        .repo
        .tasks_by_track(boot.track_id.as_str())
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
        "SELECT count(*) FROM events WHERE kind='task.context_advanced' AND scope_track=?1",
    )
    .bind(boot.track_id.as_str())
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
        planner_identity(&boot),
        json!({"kind":"task", "payload":forbidden, "if_doc_rev":read(&boot).await["docRev"]}),
    )
    .await
    .expect_err("planner cannot release");
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
        sqlx::query("UPDATE tasks SET status='running' WHERE track_id=?1 AND key='withdraw-ready'")
            .bind(boot.track_id.as_str())
            .execute(&boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
        patch_policy(&boot, policy).await;
        declaration["ready"] = json!(false);
        user_upsert(&boot, &id, rev, declaration).await;
        let stale: Option<i64> = sqlx::query_scalar(
            "SELECT context_stale_at_ms FROM tasks WHERE track_id=?1 AND key='withdraw-ready'",
        )
        .bind(boot.track_id.as_str())
        .fetch_one(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
        assert!(stale.is_some(), "policy {policy}");
    }
}

#[tokio::test]
async fn inflight_ready_withdrawal_emits_once_and_surfaces_on_both_reads() {
    let boot = new_boot().await;
    let declaration = task("adopted-ready");
    let (id, _) = upsert(&boot, None, declaration.clone()).await;
    sqlx::query("UPDATE tasks SET status='running' WHERE track_id=?1 AND key='adopted-ready'")
        .bind(boot.track_id.as_str())
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
    let before_events = boot.repo.events_since(0, i64::MAX).await.unwrap().len();
    let mut withdrawn = declaration;
    withdrawn["ready"] = json!(false);
    upsert(&boot, Some((&id, rev)), withdrawn).await;

    let stale: Option<i64> = sqlx::query_scalar(
        "SELECT context_stale_at_ms FROM tasks WHERE track_id=?1 AND key='adopted-ready'",
    )
    .bind(boot.track_id.as_str())
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
    sqlx::query("UPDATE tasks SET status='running' WHERE track_id=?1 AND key='release-edge'")
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let mut released = task("release-edge");
    released["released_by_user"] = json!(true);
    user_upsert(&boot, &id, rev, released).await;
    let stale: Option<i64> = sqlx::query_scalar(
        "SELECT context_stale_at_ms FROM tasks WHERE track_id=?1 AND key='release-edge'",
    )
    .bind(boot.track_id.as_str())
    .fetch_one(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    assert_eq!(stale, None, "false->true is never a withdrawal");

    sqlx::query(
        "UPDATE tasks SET decl_released_by_user=1 WHERE track_id=?1 AND key='release-edge'",
    )
    .bind(boot.track_id.as_str())
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
        "SELECT context_stale_at_ms FROM tasks WHERE track_id=?1 AND key='release-edge'",
    )
    .bind(boot.track_id.as_str())
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
async fn in_flight_reference_target_deletion_warns_on_both_reads_without_declaration_edit() {
    let boot = new_boot().await;
    let target = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        planner_identity(&boot),
        json!({"kind": "prose", "markdown": "old target", "if_doc_rev": read(&boot).await["docRev"]}),
    )
    .await
    .unwrap();
    let target_id = target["id"].as_str().unwrap().to_string();
    let target_rev = target["rev"].as_u64().unwrap();
    user_delete(&boot, &target_id, target_rev).await;

    let declaration = {
        let mut value = task("same-write-ref");
        value["refs"] = json!([format!("neige://wave/{}#{target_id}", boot.track_id)]);
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
    let current = TrackReportPayload {
        schema_version: TrackReportPayload::SCHEMA_VERSION,
        doc_rev: before["docRev"].as_u64().unwrap(),
        summary: String::new(),
        body: body.clone(),
        blocks: Some(blocks),
    };
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query(
        "UPDATE cards SET payload=json(?1),body_crdt=NULL WHERE track_id=?2 AND kind='track-report'",
    )
    .bind(serde_json::to_string(&TrackReportPayload::initial()).unwrap())
    .bind(boot.track_id.as_str())
    .execute(&pool)
    .await
    .unwrap();
    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let report = boot
        .repo
        .cards_by_track(boot.track_id.as_str())
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.kind == "track-report")
        .unwrap();
    persist_report(
        boot.repo.as_ref(),
        &boot.ctx.events,
        &boot.ctx.write,
        ActorId::Kernel,
        EditAuthor::Kernel,
        track,
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

    sqlx::query("UPDATE tasks SET status='running' WHERE track_id=?1 AND key='same-write-ref'")
        .bind(boot.track_id.as_str())
        .execute(&pool)
        .await
        .unwrap();

    let snapshot = read(&boot).await;
    let referring_id = task_id;
    let body = format!(
        "<!-- neige:{referring_id} -->\n{}",
        render_fence("task", &declaration)
    );
    call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        planner_identity(&boot),
        json!({"body": body, "if_doc_rev": snapshot["docRev"]}),
    )
    .await
    .expect("delete referenced target");
    let mcp = read(&boot).await;
    let rest = rest_read(&boot).await;
    for snapshot in [&mcp, &rest] {
        assert!(diagnostic_contains(
            snapshot,
            "same-write-ref",
            "does not exist"
        ));
        assert!(diagnostic_contains(
            snapshot,
            "same-write-ref",
            "is in flight (running) and cannot be withdrawn immediately"
        ));
    }
    let error = TaskContextMonitor::new(
        boot.repo.clone(),
        boot.ctx.events.clone(),
        boot.ctx.write.clone(),
    )
    .resolve_task_closure(boot.track_id.as_str(), "same-write-ref")
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
        planner_identity(&boot),
        json!({"kind":"prose","markdown":"leaf","if_doc_rev":read(&boot).await["docRev"]}),
    )
    .await
    .unwrap();
    let target_id = target["id"].as_str().unwrap().to_string();
    let target_rev = target["rev"].as_u64().unwrap();
    let middle = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        planner_identity(&boot),
        json!({"kind":"prose","markdown":format!("[leaf](neige://wave/{}#{target_id})",boot.track_id),"if_doc_rev":read(&boot).await["docRev"]}),
    )
    .await
    .unwrap();
    let middle_id = middle["id"].as_str().unwrap().to_string();
    let mut declaration = task("depth-two-absent");
    declaration["refs"] = json!([format!("neige://wave/{}#{middle_id}", boot.track_id)]);
    upsert(&boot, None, declaration).await;
    user_delete(&boot, &target_id, target_rev).await;
    let error = TaskContextMonitor::new(
        boot.repo.clone(),
        boot.ctx.events.clone(),
        boot.ctx.write.clone(),
    )
    .resolve_task_closure(boot.track_id.as_str(), "depth-two-absent")
    .await
    .expect_err("deleted depth-two production block id must be classified");
    assert!(matches!(error, ResolveError::ReferencedBlockAbsent(_)));
}

#[tokio::test]
async fn user_declared_release_withdrawal_marks_inflight_even_when_schedulable() {
    let boot = new_boot().await;
    let mut declared = task("user-release-withdrawal");
    let (id, _) = upsert(&boot, None, declared.clone()).await;
    sqlx::query("UPDATE tasks SET status='running',declared_by='user',decl_released_by_user=1 WHERE track_id=?1 AND key=?2")
        .bind(boot.track_id.as_str())
        .bind("user-release-withdrawal")
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    declared["released_by_user"] = json!(false);
    declared["declared_by"] = json!("user");
    let (card_id, payload_json, bytes): (String, String, Vec<u8>) = sqlx::query_as(
        "SELECT id,json(payload),body_crdt FROM cards WHERE track_id=?1 AND kind='track-report'",
    )
    .bind(boot.track_id.as_str())
    .fetch_one(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
    doc.upsert_block(Some(&id), "task", &render_fence("task", &declared))
        .unwrap();
    let mut payload: calm_server::track_report::TrackReportPayload =
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
        "SELECT context_stale_at_ms FROM tasks WHERE track_id=?1 AND key='user-release-withdrawal'",
    )
    .bind(boot.track_id.as_str())
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
    sqlx::query("UPDATE tasks SET status='done' WHERE track_id=?1 AND key='finished'")
        .bind(boot.track_id.as_str())
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
    sqlx::query("UPDATE tasks SET status='running' WHERE track_id=?1 AND key='deleted-running'")
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_DELETE,
        planner_identity(&boot),
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
        "SELECT context_stale_at_ms FROM tasks WHERE track_id=?1 AND key='deleted-running'",
    )
    .bind(boot.track_id.as_str())
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
        planner_identity(&boot),
        json!({"id":id, "if_rev":rev}),
    )
    .await
    .expect("planner deletion is permitted before user tombstone coverage");

    let (id, rev) = upsert(&boot, None, task("phoenix")).await;

    // User delete normalizes the declaration to a tombstone.
    let state = route_state(&boot).await;
    let _ = delete_block(
        State(RouteState::from_ref(&state)),
        principal(),
        Actor("user".into()),
        Path((boot.track_id.to_string(), id.clone())),
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
        Path((boot.track_id.to_string(), id)),
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
async fn acceptance_1_report_spawn_only_edit_emits_plan_updated_and_changes_frozen_route_column() {
    let boot = new_boot().await;
    let mut rx = boot.ctx.events.subscribe();
    let (id, rev) = upsert(&boot, None, task("events")).await;
    let events = [
        rx.recv().await.unwrap(),
        rx.recv().await.unwrap(),
        rx.recv().await.unwrap(),
    ];
    assert!(matches!(events[0].event, Event::CardUpdated(_)));
    assert!(matches!(events[1].event, Event::TrackReportEdited { .. }));
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
    assert!(matches!(events[1].event, Event::TrackReportEdited { .. }));
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));

    let mut sub_track = task("events");
    sub_track["spawn"] = json!("sub-wave");
    let current = read(&boot).await;
    let rev = current["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["id"] == id)
        .unwrap()["rev"]
        .as_u64()
        .unwrap();
    upsert(&boot, Some((&id, rev)), sub_track).await;
    let events = [
        rx.recv().await.unwrap(),
        rx.recv().await.unwrap(),
        rx.recv().await.unwrap(),
    ];
    assert!(matches!(events[0].event, Event::CardUpdated(_)));
    assert!(matches!(events[1].event, Event::TrackReportEdited { .. }));
    match &events[2].event {
        Event::PlanUpdated { changed_keys, .. } => assert_eq!(changed_keys, &["events"]),
        other => panic!("expected spawn-only PlanUpdated, got {other:?}"),
    }
    let spawn: String =
        sqlx::query_scalar("SELECT spawn FROM tasks WHERE track_id=?1 AND key='events'")
            .bind(boot.track_id.as_str())
            .fetch_one(&boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    assert_eq!(spawn, "sub-wave");
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
    sqlx::query("UPDATE tracks SET automation_policy='declare-and-wait' WHERE id=?1")
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let mut changed = task("waiting");
    changed["goal"] = json!("reevaluate policy");
    upsert(&boot, Some((&id, rev)), changed).await;
    assert_diagnosed_on_both_reads(&boot, "waiting", "requires user release").await;

    // planner_task_ceiling
    let boot = new_boot().await;
    let (id, rev) = upsert(&boot, None, task("ceiling")).await;
    sqlx::query("UPDATE tracks SET planner_task_ceiling=0 WHERE id=?1")
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let mut changed = task("ceiling");
    changed["goal"] = json!("reevaluate ceiling");
    upsert(&boot, Some((&id, rev)), changed).await;
    assert_diagnosed_on_both_reads(&boot, "ceiling", "ceiling of 0").await;

    // cross-area reference
    let boot = new_boot().await;
    let (id, rev) = upsert(&boot, None, task("cross")).await;
    let other_area = boot
        .repo
        .area_create(calm_server::model::NewArea {
            name: "other".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let other_track = boot
        .repo
        .track_create(calm_server::model::NewTrack {
            template_input: None,
            area_id: other_area.id,
            title: "other".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let mut changed = task("cross");
    changed["refs"] = json!([format!("neige://wave/{}#b_dead", other_track.id)]);
    upsert(&boot, Some((&id, rev)), changed).await;
    assert_diagnosed_on_both_reads(&boot, "cross", "cross-area").await;
}

async fn task_bytes(boot: &Boot) -> Vec<String> {
    sqlx::query_scalar("SELECT json_object('key',key,'kind',kind,'goal',goal,'context',context_json,'acceptance',acceptance_criteria,'cwd',cwd,'depends',depends_on_json,'priority',priority,'gate',gate_json,'declared_by',declared_by,'decl_ready',decl_ready,'decl_released_by_user',decl_released_by_user,'context_verify_failures',context_verify_failures,'status',status,'status_detail',status_detail,'worker',worker_card_id,'gate_result',gate_result_json,'gate_attempt',gate_attempt,'gate_pid',gate_pid,'gate_pid_starttime',gate_pid_starttime,'gate_pid_boot_id',gate_pid_boot_id,'finished',finished_at_ms,'deadline',running_deadline_ms,'claim_context',claim_context_json,'context_stale',context_stale_at_ms,'closure_truncated',context_closure_truncated) FROM tasks WHERE track_id=?1 ORDER BY key")
        .bind(boot.track_id.as_str()).fetch_all(&boot.repo.sqlite_pool().unwrap()).await.unwrap()
}

#[tokio::test]
async fn rebuild_matches_incremental_bytes_after_adversarial_edit_sequence() {
    let boot = new_boot().await;
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,acceptance_criteria,cwd,depends_on_json,priority,gate_json,status,status_detail,declared_by,created_at_ms,updated_at_ms) VALUES(?1,?2,'flight','codex','goal flight','{\"key\":\"flight\"}','accept flight','/flight','[]',3,'{\"steps\":[{\"name\":\"accept\",\"cmd\":\"true\"}]}','running','owned-byte','spec',0,0)")
        .bind(format!("{}:flight", boot.track_id)).bind(boot.track_id.as_str())
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
        planner_identity(&boot),
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
    // Damage materialized rows in two distinct ways so rebuild must recreate
    // a surviving pending row and repair declaration bytes on the in-flight row.
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,depends_on_json,priority,status,declared_by,created_at_ms,updated_at_ms) VALUES(?1,?2,'undeclared-damage','codex','damage','{}','[]',0,'pending','spec',0,0)")
        .bind(format!("{}:undeclared-damage", boot.track_id))
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_ne!(task_bytes(&boot).await, before, "damage must be observable");
    sqlx::query("DELETE FROM tasks WHERE track_id=?1 AND key='b'")
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let mut tx = boot.repo.sqlite_pool().unwrap().begin().await.unwrap();
    tasks_rebuild_tx(&mut tx, boot.track_id.as_str())
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
            sqlx::query("UPDATE tracks SET automation_policy='declare-and-wait' WHERE id=?1")
                .bind(boot.track_id.as_str())
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
            "UPDATE tasks SET status='dispatched',claim_context_json='[]' WHERE track_id=?1",
        )
        .bind(boot.track_id.as_str())
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
            project_tasks_tx(&mut tx, boot.track_id.as_str(), &declarations, &diagnostics)
                .await
                .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            incremental.kernel_events.len(),
            1,
            "incremental withdrawal event"
        );
        let stale: i64 =
            sqlx::query_scalar("SELECT context_stale_at_ms FROM tasks WHERE track_id=?1")
                .bind(boot.track_id.as_str())
                .fetch_one(&boot.repo.sqlite_pool().unwrap())
                .await
                .unwrap();
        let incremental_bytes = task_bytes(&boot).await;

        let (card_id, payload_json, bytes): (String, String, Vec<u8>) = sqlx::query_as(
            "SELECT id,json(payload),body_crdt FROM cards WHERE track_id=?1 AND kind='track-report'",
        )
        .bind(boot.track_id.as_str())
        .fetch_one(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
        let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
        doc.upsert_block(Some(&id), "task", &render_fence("task", &withdrawn))
            .unwrap();
        let mut payload: TrackReportPayload = serde_json::from_str(&payload_json).unwrap();
        payload.body = doc.project().unwrap().1;
        payload.blocks = Some(doc.blocks_snapshot().unwrap());
        sqlx::query("UPDATE cards SET payload=json(?1),body_crdt=?2 WHERE id=?3")
            .bind(serde_json::to_string(&payload).unwrap())
            .bind(doc.to_bytes())
            .bind(card_id)
            .execute(&boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
        sqlx::query("UPDATE tasks SET context_stale_at_ms=NULL WHERE track_id=?1")
            .bind(boot.track_id.as_str())
            .execute(&boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
        let rebuilt = rebuild(&boot).await;
        assert_eq!(rebuilt.kernel_events.len(), 1, "rebuild withdrawal event");
        sqlx::query("UPDATE tasks SET context_stale_at_ms=?1 WHERE track_id=?2")
            .bind(stale)
            .bind(boot.track_id.as_str())
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
        sqlx::query("UPDATE tasks SET status='running',status_detail='owned' WHERE track_id=?1 AND key='flight'")
            .bind(boot.track_id.as_str()).execute(&boot.repo.sqlite_pool().unwrap()).await.unwrap();
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
            "UPDATE tasks SET status='running' WHERE track_id=?1 AND key='non-context-drift'",
        )
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
        if field == "declared_by" {
            sqlx::query(
                "UPDATE tasks SET declared_by='user' WHERE track_id=?1 AND key='non-context-drift'",
            )
            .bind(boot.track_id.as_str())
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
            "SELECT context_stale_at_ms FROM tasks WHERE track_id=?1 AND key='non-context-drift'",
        )
        .bind(boot.track_id.as_str())
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
async fn canonical_gate_and_context_are_semantically_equal_to_block_declaration() {
    let boot = new_boot().await;
    let (id, rev) = upsert(&boot, None, task("flight")).await;
    sqlx::query("UPDATE tasks SET status='running',gate_json=?1,context_json=?2 WHERE track_id=?3 AND key='flight'")
        .bind(r#"{"steps":[{"name":"accept","cmd":"true"}]}"#)
        .bind(r#"{"key":"flight"}"#)
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap()).await.unwrap();
    upsert(&boot, Some((&id, rev)), task("flight")).await;
    assert!(
        !diagnostic_contains(&read(&boot).await, "flight", "declaration changes"),
        "equivalent JSON spelling must not create a stale diagnostic"
    );
}

#[tokio::test]
async fn unknown_dependencies_treat_inflight_rows_as_known() {
    let boot = new_boot().await;
    upsert(&boot, None, task("old-running")).await;
    sqlx::query("UPDATE tasks SET status='running' WHERE track_id=?1 AND key='old-running'")
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let mut known = task("uses-running");
    known["depends_on"] = json!(["old-running"]);
    upsert(&boot, None, known).await;
    assert!(keys(&boot).await.contains(&"uses-running".into()));
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
        planner_identity(&boot),
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
async fn user_tombstone_does_not_derive_wait_and_policy_remains_independent() {
    let boot = new_boot().await;
    let (denied, denied_rev) = upsert(&boot, None, task("denied")).await;
    user_delete(&boot, &denied, denied_rev).await;
    upsert(&boot, None, task("replacement")).await;
    assert_eq!(keys(&boot).await, ["replacement"]);
    assert!(
        rest_read(&boot).await["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["id"] == denied && b["payload"]["tombstoned_by"] == "user")
    );
    patch_policy(&boot, "declare-and-wait").await;
    assert_diagnosed_on_both_reads(&boot, "replacement", "requires user release").await;
    patch_policy(&boot, "auto-declare").await;
    assert!(
        rest_read(&boot).await["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["id"] == denied && b["payload"]["tombstoned_by"] == "user")
    );
    assert_eq!(keys(&boot).await, ["replacement"]);
}

#[tokio::test]
async fn explicit_wait_policy_removes_unreleased_pending_rows_with_readable_reason() {
    let boot = new_boot().await;
    let (vetoed, vetoed_rev) = upsert(&boot, None, task("vetoed")).await;
    upsert(&boot, None, task("collateral")).await;
    assert_eq!(keys(&boot).await, ["collateral", "vetoed"]);
    user_delete(&boot, &vetoed, vetoed_rev).await;
    assert_eq!(keys(&boot).await, ["collateral"]);
    patch_policy(&boot, "declare-and-wait").await;
    assert_diagnosed_on_both_reads(&boot, "collateral", "requires user release").await;
}

#[tokio::test]
async fn invalid_task_payload_deletes_pending_row_with_readable_reason() {
    let boot = new_boot().await;
    let (id, rev) = upsert(&boot, None, task("invalidated")).await;
    let mut invalid = task("invalidated");
    invalid["acceptance"] = json!("");
    let (card_id, payload_json, bytes): (String, String, Vec<u8>) = sqlx::query_as(
        "SELECT id,json(payload),body_crdt FROM cards WHERE track_id=?1 AND kind='track-report'",
    )
    .bind(boot.track_id.as_str())
    .fetch_one(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
    doc.upsert_block(Some(&id), "task", &render_fence("task", &invalid))
        .unwrap();
    let mut payload: calm_server::track_report::TrackReportPayload =
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
        "SELECT id,json(payload),body_crdt FROM cards WHERE track_id=?1 AND kind='track-report'",
    )
    .bind(boot.track_id.as_str())
    .fetch_one(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    let mut invalid = task("invalid-gate");
    invalid["gate"] = Value::Null;
    let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
    doc.upsert_block(Some(&id), "task", &render_fence("task", &invalid))
        .unwrap();
    let mut payload: calm_server::track_report::TrackReportPayload =
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
    sqlx::query("UPDATE tracks SET planner_task_ceiling=2 WHERE id=?1")
        .bind(boot.track_id.as_str())
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
    sqlx::query("UPDATE tracks SET planner_task_ceiling=1 WHERE id=?1")
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    upsert(&boot, None, task("first")).await;
    let (second, _) = upsert(&boot, None, task("second")).await;
    assert_eq!(keys(&boot).await, ["first"]);
    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_MOVE,
        planner_identity(&boot),
        json!({"id":second,"to_index":0,"if_doc_rev":read(&boot).await["docRev"]}),
    )
    .await
    .unwrap();
    assert_eq!(keys(&boot).await, ["second"]);
    assert_diagnosed_on_both_reads(&boot, "first", "ceiling of 1").await;
}

#[tokio::test]
async fn terminal_planner_key_does_not_consume_ceiling_capacity() {
    let boot = new_boot().await;
    sqlx::query("UPDATE tracks SET planner_task_ceiling=1 WHERE id=?1")
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,depends_on_json,priority,status,declared_by,created_at_ms,updated_at_ms) VALUES(?1,?2,'done','codex','done','{}','[]',0,'done','spec',0,0)")
        .bind(format!("{}:done", boot.track_id)).bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap()).await.unwrap();
    upsert(&boot, None, task("new-capacity")).await;
    assert_eq!(keys(&boot).await, ["done", "new-capacity"]);
    let status: String =
        sqlx::query_scalar("SELECT status FROM tasks WHERE track_id=?1 AND key='new-capacity'")
            .bind(boot.track_id.as_str())
            .fetch_one(&boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    assert_eq!(status, "pending");
}

/// #1070 regression: handler-only route fixtures do not attach a live
/// dispatcher, so the delete-to-policy-PATCH window keeps `collateral`
/// pending and the wait-policy projection must delete it.
#[tokio::test]
async fn handler_fixture_keeps_pending_row_unclaimed_until_policy_patch() {
    let boot = new_boot().await;
    let (vetoed, vetoed_rev) = upsert(&boot, None, task("vetoed")).await;
    upsert(&boot, None, task("collateral")).await;
    user_delete(&boot, &vetoed, vetoed_rev).await;
    let status: String =
        sqlx::query_scalar("SELECT status FROM tasks WHERE track_id=?1 AND key='collateral'")
            .bind(boot.track_id.as_str())
            .fetch_one(&boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    assert_eq!(
        status, "pending",
        "handler fixture must not run a scheduler"
    );

    patch_policy(&boot, "declare-and-wait").await;
    assert_diagnosed_on_both_reads(&boot, "collateral", "requires user release").await;
}
