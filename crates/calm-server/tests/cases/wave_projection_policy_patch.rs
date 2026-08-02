use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::NewCard;
use calm_server::model::{NewCove, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, DaemonClient};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn boot() -> (AppState, String, Arc<dyn Repo>) {
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "policy".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "policy".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-policy-patch-plugins"),
            Vec::new(),
            events,
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        )),
        Arc::new(calm_server::state::CodexClient::new_stub()),
        None,
        None,
    );
    (state, wave.id.to_string(), repo)
}

fn app(state: AppState) -> axum::Router {
    routes::waves::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state)
}

async fn patch(
    state: AppState,
    wave_id: &str,
    actor: Option<&str>,
    body: Value,
) -> axum::http::Response<Body> {
    let mut request = Request::builder()
        .method("PATCH")
        .uri(format!("/api/waves/{wave_id}"))
        .header("content-type", "application/json");
    if let Some(actor) = actor {
        request = request.header("X-Calm-Actor", actor);
    }
    app(state)
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

async fn columns(repo: &Arc<dyn Repo>, wave_id: &str) -> (Option<i64>, Option<String>) {
    sqlx::query_as("SELECT spec_task_ceiling, automation_policy FROM waves WHERE id = ?1")
        .bind(wave_id)
        .fetch_one(&repo.sqlite_pool().unwrap())
        .await
        .unwrap()
}

async fn event_count(repo: &Arc<dyn Repo>) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(&repo.sqlite_pool().unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn policy_only_patch_is_not_short_circuited_and_clear_nulls_work() {
    let (state, wave_id, repo) = boot().await;
    assert_eq!(columns(&repo, &wave_id).await, (Some(32), None));
    let before = event_count(&repo).await;

    let response = patch(
        state.clone(),
        &wave_id,
        None,
        json!({"automation_policy": "declare-and-wait"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        columns(&repo, &wave_id).await,
        (Some(32), Some("declare-and-wait".into()))
    );
    assert_eq!(event_count(&repo).await, before + 1);

    let response = patch(
        state.clone(),
        &wave_id,
        None,
        json!({"spec_task_ceiling": 7}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(columns(&repo, &wave_id).await.0, Some(7));

    let response = patch(
        state,
        &wave_id,
        None,
        json!({"automation_policy": null, "spec_task_ceiling": null}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(columns(&repo, &wave_id).await, (None, None));
}

#[tokio::test]
async fn non_user_policy_patches_are_forbidden_without_rows_or_events() {
    let (state, wave_id, repo) = boot().await;
    let original = columns(&repo, &wave_id).await;
    let before = event_count(&repo).await;

    for body in [
        json!({"automation_policy": "auto-declare"}),
        json!({"spec_task_ceiling": 99}),
    ] {
        let response = patch(state.clone(), &wave_id, Some("ai:codex"), body).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(columns(&repo, &wave_id).await, original);
        assert_eq!(event_count(&repo).await, before);
    }
}

#[tokio::test]
async fn invalid_policy_values_are_rejected() {
    let (state, wave_id, repo) = boot().await;
    for body in [
        json!({"automation_policy": "unsafe"}),
        json!({"spec_task_ceiling": -1}),
    ] {
        let response = patch(state.clone(), &wave_id, None, body).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert_eq!(columns(&repo, &wave_id).await, (Some(32), None));
}

#[tokio::test]
async fn tightening_policy_immediately_deletes_pending_projection_and_emits_plan_updated() {
    let (state, wave_id, repo) = boot().await;
    let block = calm_types::wave_report::ReportBlock {
        id: "b_policy".into(),
        rev: 1,
        kind: "task".into(),
        payload: json!({"key":"queued","kind":"codex","goal":"queued goal",
            "acceptance":"done","no_gate_reason":"not needed","declared_by":"spec","ready":true}),
    };
    let mut payload = calm_server::wave_report::WaveReportPayload::new(
        "",
        calm_types::report_blocks::flat_text(&block),
    );
    payload.blocks = Some(vec![block]);
    let report = repo
        .card_create(NewCard {
            wave_id: wave_id.clone().into(),
            title: None,
            kind: "wave-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(calm_server::wave_report::WaveReportPayload::initial())
                .unwrap(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET payload=?1 WHERE id=?2")
        .bind(serde_json::to_string(&payload).unwrap())
        .bind(report.id.as_str())
        .execute(&repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tasks(id,wave_id,key,kind,goal,context_json,depends_on_json,priority,status,declared_by,origin,created_at_ms,updated_at_ms) VALUES(?1,?2,'queued','codex','queued goal','{}','[]',0,'pending','spec','block',0,0)")
        .bind(format!("{wave_id}:queued")).bind(&wave_id).execute(&repo.sqlite_pool().unwrap()).await.unwrap();

    let response = patch(
        state,
        &wave_id,
        None,
        json!({"automation_policy":"declare-and-wait"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE wave_id=?1")
        .bind(&wave_id)
        .fetch_one(&repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_eq!(count, 0);
    let plan_events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM events WHERE kind='plan.updated'")
            .fetch_one(&repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    assert_eq!(plan_events, 1);
}
