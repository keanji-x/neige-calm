//! A fork copies every `report_series` row of the source track inside the fork's own transaction;
//! block ids survive the fork, so the rows keep their identity.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{self, AuthConfig, AuthState, SESSION_COOKIE};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewArea, NewCard, NewTrack};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_report::TrackReportPayload;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::report_series_fixture::{Row, SOURCE, rows_for};

struct Boot {
    app: axum::Router,
    repo: Arc<dyn Repo>,
    area_id: String,
    source_track_id: String,
    cookie: String,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "series-fork".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let source = repo
        .track_create(NewTrack {
            area_id: area.id.clone(),
            title: "source".into(),
            sort: None,
            cwd: tmp.path().to_string_lossy().into_owned(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    repo.card_create(NewCard {
        track_id: source.id.clone(),
        kind: "track-report".into(),
        sort: Some(-1.0),
        payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
        title: None,
    })
    .await
    .unwrap();
    let events = EventBus::new();
    let card_roles = CardRoleCache::new();
    let track_areas = calm_server::track_area_cache::TrackAreaCache::new();
    repo.seed_track_area_cache(&track_areas).await.unwrap();
    let write = calm_server::state::WriteContext::new(card_roles.clone(), track_areas.clone());
    let state = AppState::from_parts(
        repo.clone(),
        events,
        Arc::new(DaemonClient {
            data_dir: tmp.path().to_path_buf(),
            proc_supervisor_sock: None,
        }),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            tmp.path().join("plugins"),
            Vec::new(),
            EventBus::new(),
            write,
        )),
        {
            let mut codex = CodexClient::new_stub();
            codex.codex_bin = "/nonexistent-codex-bin-series-fork".into();
            Arc::new(codex)
        },
        Some(card_roles),
        Some(track_areas),
    )
    .with_workspace_root(tmp.path().join("workspaces"));
    let auth_state = AuthState::new(AuthConfig {
        username: Some("alice".into()),
        password: Some("hunter2".into()),
        dev_autologin: false,
        display_name: "alice".into(),
    });
    let protected = routes::protected_router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            auth_state.clone(),
            auth::require_session,
        ));
    let app = axum::Router::new()
        .merge(protected)
        .merge(routes::public_router())
        .with_state(state)
        .merge(auth::router().with_state(auth_state));
    let cookie = login(&app).await;
    Boot {
        app,
        repo,
        area_id: area.id.to_string(),
        source_track_id: source.id.to_string(),
        cookie,
        _tmp: tmp,
    }
}

async fn login(app: &axum::Router) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"username":"alice", "password":"hunter2"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let raw = response.headers()[header::SET_COOKIE].to_str().unwrap();
    let cookie = raw.split(';').next().unwrap().to_string();
    assert!(cookie.starts_with(&format!("{SESSION_COOKIE}=")));
    cookie
}

async fn request_json(
    app: &axum::Router,
    cookie: &str,
    method: &str,
    uri: String,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, cookie);
    let body = match body {
        Some(body) => {
            request = request.header("content-type", "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn insert_row(repo: &dyn Repo, track_id: &str, block_id: &str, hash: &str, pinned: bool) {
    let pool = repo.sqlite_pool().unwrap();
    sqlx::query(concat!(
        "INSERT INTO report_series ",
        "(track_id, block_id, request_hash, status, reason, as_of, ",
        " resolved_at, pinned, summary, data) ",
        "VALUES (?1, ?2, ?3, 'ok', NULL, '2026-09-10', 1000, ?4, ?5, ?6)"
    ))
    .bind(track_id)
    .bind(block_id)
    .bind(hash)
    .bind(pinned)
    .bind(json!({ "series": [{ "asset": "US:NVDA", "status": "ok", "n": 2 }] }).to_string())
    .bind(json!({ "series": [{ "asset": "US:NVDA", "status": "ok", "points": [[0, 1.0], [86400000, 2.0]] }] }).to_string())
    .execute(&pool)
    .await
    .expect("insert report_series row");
}

fn without_track(mut rows: Vec<Row>) -> Vec<Row> {
    rows.sort_by(|a, b| (&a.block_id, &a.request_hash).cmp(&(&b.block_id, &b.request_hash)));
    rows
}

#[tokio::test]
async fn fork_copies_report_series_rows() {
    let boot = boot().await;
    let (status, report) = request_json(
        &boot.app,
        &boot.cookie,
        "GET",
        format!("/api/tracks/{}/report", boot.source_track_id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let (status, created) = request_json(
        &boot.app,
        &boot.cookie,
        "POST",
        format!("/api/tracks/{}/report/blocks", boot.source_track_id),
        Some(json!({
            "kind": "chart.series",
            "payload": { "source": SOURCE, "series": ["US:NVDA"], "as_of": "2026-09-10" },
            "ifDocRev": report["docRev"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seed chart.series block: {created}");
    let block_id = created["id"].as_str().unwrap().to_string();
    // Two rows for the block: the current hash (pinned) and an old one
    // (unpinned) — a fork copies ALL rows, not just pinned ones.
    insert_row(
        boot.repo.as_ref(),
        &boot.source_track_id,
        &block_id,
        "h-current",
        true,
    )
    .await;
    insert_row(
        boot.repo.as_ref(),
        &boot.source_track_id,
        &block_id,
        "h-old",
        false,
    )
    .await;
    let source_before = without_track(rows_for(boot.repo.as_ref(), &boot.source_track_id).await);
    assert_eq!(source_before.len(), 2);

    let (status, forked) = request_json(
        &boot.app,
        &boot.cookie,
        "POST",
        "/api/tracks".into(),
        // Omitted `cwd`: a managed workspace under the pinned root.
        Some(json!({
            "area_id": boot.area_id,
            "title": "fork target",
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_track_id,
        })),
    )
    .await;
    // The stub daemon may fail post-commit; the track and its rows landed.
    assert!(
        status == StatusCode::CREATED || status == StatusCode::INTERNAL_SERVER_ERROR,
        "fork: {status} {forked}"
    );
    let tracks = boot.repo.tracks_by_area(&boot.area_id).await.unwrap();
    let target = tracks
        .iter()
        .find(|t| t.id.as_str() != boot.source_track_id)
        .expect("the forked track exists");

    let copied = without_track(rows_for(boot.repo.as_ref(), target.id.as_str()).await);
    assert_eq!(copied, source_before, "every row travels, byte for byte");
    let source_after = without_track(rows_for(boot.repo.as_ref(), &boot.source_track_id).await);
    assert_eq!(source_after, source_before, "the source is untouched");
    let (status, target_report) = request_json(
        &boot.app,
        &boot.cookie,
        "GET",
        format!("/api/tracks/{}/report", target.id.as_str()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{target_report}");
    assert!(
        target_report["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["id"] == json!(block_id)),
        "the block keeps its id in the fork: {target_report}"
    );
}
