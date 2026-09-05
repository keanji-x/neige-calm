//! Public recovery reads retain logical identity and current blockers.
use crate::mcp_track_report::{boot, call_tool, planner_identity};
use crate::task_recovery::{current, declaration, declare, finish, recovery_args};
use calm_server::ids::ActorId;
use calm_server::model::{NewCard, NewTrack};
use calm_server::task_recovery::task_recovery_view;
use calm_server::track_report::{TrackReportPayload, persist_report, resolve_report_for_track};
use serde_json::{Value, json};

async fn rest_attempts(
    boot: &crate::mcp_track_report::Boot,
    key: &str,
    expected: axum::http::StatusCode,
) -> Value {
    use axum::{Extension, body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let response = calm_server::routes::task_recovery::router()
        .with_state(crate::task_projection_acceptance::route_state(boot).await)
        .layer(Extension(crate::task_projection_acceptance::principal()))
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/tracks/{}/tasks/{key}/attempts",
                    boot.track_id
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(status, expected, "{}", String::from_utf8_lossy(&bytes));
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn task_recovery_list_keeps_absent_projection_with_ready_blocker() {
    let boot = boot().await;
    let (block, revision) = declare(&boot, declaration("b", &[])).await;
    let b = current(&boot, "b").await;
    finish(&boot, &b, false).await;
    let receipt = call_tool(
        &boot,
        "calm.plan.recover",
        planner_identity(&boot),
        recovery_args(&b, "read-b"),
    )
    .await
    .unwrap();
    let mut withdrawn = declaration("b", &[]);
    withdrawn["ready"] = json!(false);
    call_tool(
        &boot,
        "calm.report.blocks.upsert",
        planner_identity(&boot),
        json!({"id":block,"kind":"task","payload":withdrawn,"if_rev":revision}),
    )
    .await
    .unwrap();
    let list = call_tool(&boot, "calm.plan.list", planner_identity(&boot), json!({}))
        .await
        .unwrap();
    let entry = list["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["key"] == "b")
        .expect("current allocation must remain visible without a pending row");
    assert_eq!(entry["attempt_id"], receipt["attempt_id"]);
    assert_eq!(entry["status"], "awaiting_projection");
    assert!(entry["blocking_reason"].as_str().unwrap().contains("ready"));
    let view = serde_json::to_value(
        task_recovery_view(
            boot.repo.as_ref(),
            boot.track_id.as_str(),
            "b",
            ActorId::User,
            calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(view["current"]["blocking_reason"], entry["blocking_reason"]);
    assert!(view["attempts"][0]["blocking_reason"].is_null());
}

#[tokio::test]
async fn task_recovery_history_retains_current_dependency_blocker() {
    let boot = boot().await;
    declare(&boot, declaration("b", &[])).await;
    declare(&boot, declaration("c", &["b"])).await;
    let b = current(&boot, "b").await;
    finish(&boot, &b, false).await;
    let report = call_tool(
        &boot,
        "calm.report.read",
        planner_identity(&boot),
        json!({}),
    )
    .await
    .unwrap();
    let reason = report["taskDiagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["key"] == "c")
        .unwrap()["pendingReason"]["message"]
        .clone();
    assert!(reason.as_str().unwrap().contains("`b`"));
    let view = serde_json::to_value(
        task_recovery_view(
            boot.repo.as_ref(),
            boot.track_id.as_str(),
            "c",
            ActorId::User,
            calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(view["current"]["blocking_reason"], reason);
    let rest = rest_attempts(&boot, "c", axum::http::StatusCode::OK).await;
    assert_eq!(rest["current"]["blocking_reason"], reason);
}

#[tokio::test]
async fn task_recovery_deleted_frozen_reference_denies_only_affected_capability() {
    let boot = boot().await;
    let target = boot
        .repo
        .track_create(NewTrack {
            template_input: None,
            area_id: boot.area_id.clone(),
            title: "reference source".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    boot.repo
        .card_create(NewCard {
            track_id: target.id.clone(),
            title: None,
            kind: "track-report".into(),
            sort: None,
            payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
        })
        .await
        .unwrap();
    let (track, card, payload) = resolve_report_for_track(boot.repo.as_ref(), target.id.as_str())
        .await
        .unwrap();
    let revision = payload.doc_rev;
    let written = persist_report(
        boot.repo.as_ref(),
        &boot.ctx.events,
        &boot.ctx.write,
        ActorId::User,
        calm_server::event::EditAuthor::User,
        track,
        card,
        payload,
        TrackReportPayload::new("", "## Input\nFrozen input"),
        revision,
        None,
        None,
        false,
    )
    .await
    .unwrap();
    let block = written.payload["blocks"][0]["id"].as_str().unwrap();
    let mut b_decl = declaration("b", &[]);
    b_decl["refs"] = json!([calm_types::report_links::format_track_destination(
        target.id.as_str(),
        Some(block)
    )]);
    declare(&boot, declaration("a", &[])).await;
    declare(&boot, b_decl).await;
    let b = current(&boot, "b").await;
    finish(&boot, &b, false).await;
    boot.repo.track_delete(target.id.as_str()).await.unwrap();
    let view = task_recovery_view(
        boot.repo.as_ref(),
        boot.track_id.as_str(),
        "b",
        ActorId::User,
        calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
    )
    .await
    .expect("missing frozen input is a task capability, not a missing requested task");
    assert!(!view.recovery.allowed);
    assert!(view.recovery.reason.contains("missing"));
    assert_eq!(view.attempts.len(), 1);
    let rest = rest_attempts(&boot, "b", axum::http::StatusCode::OK).await;
    assert_eq!(rest["current"]["attempt_id"], b.id);
    assert_eq!(rest["recovery"]["allowed"], false);
    rest_attempts(&boot, "absent", axum::http::StatusCode::NOT_FOUND).await;
    let list = call_tool(
        &boot,
        "calm.plan.list",
        planner_identity(&boot),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(list["tasks"].as_array().unwrap().len(), 2);
    let missing = task_recovery_view(
        boot.repo.as_ref(),
        boot.track_id.as_str(),
        "absent",
        ActorId::User,
        calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        missing,
        calm_server::error::CalmError::NotFound(_)
    ));
}

#[tokio::test]
async fn task_recovery_plan_inventory_pages_all_current_allocations() {
    let boot = boot().await;
    let pool = boot.repo.sqlite_pool().unwrap();
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    for index in 0..130 {
        let key = format!("k{index:03}");
        sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,created_at_ms,updated_at_ms,finished_at_ms) VALUES(?1,?2,?3,'terminal','true','{}','done',1,1,1)")
            .bind(format!("{}:{key}", boot.track_id)).bind(boot.track_id.as_str()).bind(key).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    let list = call_tool(&boot, "calm.plan.list", planner_identity(&boot), json!({}))
        .await
        .unwrap();
    let keys: Vec<_> = list["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|task| task["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys.len(), 130);
    assert_eq!(keys.first(), Some(&"k000"));
    assert_eq!(keys.last(), Some(&"k129"));
    assert_eq!(
        keys.iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        130
    );
}

#[tokio::test]
async fn task_recovery_blocker_uses_live_configured_budget_like_report_read() {
    let boot = boot().await;
    declare(&boot, declaration("a", &[])).await;
    declare(&boot, declaration("b", &[])).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query("UPDATE tasks SET status='running' WHERE track_id=?1 AND key='a'")
        .bind(boot.track_id.as_str())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO settings(key,value,updated_at) VALUES(?1,'1',1) ON CONFLICT(key) DO UPDATE SET value='1',updated_at=excluded.updated_at",
    )
    .bind(calm_server::routes::settings::TASK_BUDGET_DEFAULT_KEY)
    .execute(&pool)
    .await
    .unwrap();
    let view = task_recovery_view(
        boot.repo.as_ref(),
        boot.track_id.as_str(),
        "b",
        ActorId::User,
        6,
    )
    .await
    .unwrap();
    let report = call_tool(
        &boot,
        "calm.report.read",
        planner_identity(&boot),
        json!({}),
    )
    .await
    .unwrap();
    let reason = report["taskDiagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["key"] == "b")
        .unwrap()["pendingReason"]["message"]
        .clone();
    assert_eq!(reason, "Queued 1/1");
    assert_eq!(
        serde_json::to_value(view).unwrap()["current"]["blocking_reason"],
        reason
    );
}
