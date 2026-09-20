//! `GET /api/tracks/{id}/report/series/{block_id}` through the real axum router.
//! The resolver is unstarted: a read records what it would enqueue, and the test runs the job by hand.

#![cfg(unix)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{self, AuthConfig, AuthState, SESSION_COOKIE};
use calm_server::report_series::{Enqueue, ResolveOutcome};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::report_series_fixture::{FixtureOptions, SeriesFixture, seam_fixture};

/// The production router tree, over an `AppState` whose series route resolves through the fixture's own `AppContext`.
fn app(fx: &SeriesFixture) -> (axum::Router, AuthState) {
    let state = AppState::from_parts(
        fx.boot.repo.clone(),
        fx.ctx().events.clone(),
        Arc::new(DaemonClient::new_stub()),
        fx.plugin_host.clone(),
        Arc::new(CodexClient::new_stub()),
        Some(fx.boot.card_role_cache.clone()),
        None,
    )
    .with_mcp_context(fx.ctx().clone());
    let auth_state = AuthState::new(AuthConfig {
        username: Some("alice".into()),
        password: Some("hunter2".into()),
        dev_autologin: false,
        display_name: "alice".into(),
    });
    let protected_rest = routes::protected_router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            auth_state.clone(),
            auth::require_session,
        ));
    let router = axum::Router::new()
        .merge(protected_rest)
        .merge(routes::public_router())
        .with_state(state)
        .merge(auth::router().with_state(auth_state.clone()));
    (router, auth_state)
}

async fn login(app: &axum::Router) -> String {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "username": "alice", "password": "hunter2" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "login must succeed");
    let raw = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("Set-Cookie present on login")
        .to_str()
        .unwrap();
    let first = raw.split(';').next().unwrap();
    assert!(first.starts_with(&format!("{SESSION_COOKIE}=")));
    first.to_string()
}

async fn get(app: &axum::Router, uri: &str, cookie: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|e| panic!("body is JSON ({e}): {}", String::from_utf8_lossy(&bytes)))
    };
    (status, body)
}

struct Route {
    app: axum::Router,
    cookie: String,
}

impl Route {
    async fn new(fx: &SeriesFixture) -> Self {
        let (app, _) = app(fx);
        let cookie = login(&app).await;
        Self { app, cookie }
    }

    fn uri(&self, track_id: &str, block_id: &str, query: &str) -> String {
        format!("/api/tracks/{track_id}/report/series/{block_id}{query}")
    }

    async fn get(&self, track_id: &str, block_id: &str, query: &str) -> (StatusCode, Value) {
        get(
            &self.app,
            &self.uri(track_id, block_id, query),
            &self.cookie,
        )
        .await
    }
}

async fn current_rev(fx: &SeriesFixture, block_id: &str) -> u64 {
    let read = fx.read(json!({ "resolve": { block_id: "none" } })).await;
    read["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == json!(block_id))
        .and_then(|b| b["rev"].as_u64())
        .expect("block rev")
}

/// Write the seam block and resolve it to a pinned `ok` row.
async fn resolved_seam_block(fx: &SeriesFixture) -> String {
    let seam = seam_fixture();
    let block_id = fx.write_series_block(seam["block"].clone()).await;
    fx.reply_structured(seam["reply"].clone());
    let (enqueued, outcomes) = fx.resolve_block(&block_id).await;
    assert_eq!(enqueued, Enqueue::Queued);
    assert_eq!(
        outcomes,
        vec![ResolveOutcome::Wrote {
            status: "ok".into(),
            pinned: true
        }]
    );
    block_id
}

#[tokio::test]
async fn series_route_rejects_stale_rev() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let route = Route::new(&fx).await;
    let block_id = fx.write_series_block(seam_fixture()["block"].clone()).await;
    let rev = current_rev(&fx, &block_id).await;

    let (status, body) = route
        .get(fx.track_id(), &block_id, &format!("?rev={}", rev + 1))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body, json!({ "current_rev": rev }));
    let (status, body) = route
        .get(
            fx.track_id(),
            &block_id,
            &format!("?rev={}", rev.saturating_sub(1)),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["current_rev"], rev);
    assert!(fx.resolver().recorded_outcomes().is_empty());

    let (status, body) = route.get(fx.track_id(), &block_id, "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = route.get(fx.track_id(), &block_id, "?rev=x").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = route
        .get(fx.track_id(), &block_id, &format!("?rev={rev}"))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "pending");

    let new_rev = fx
        .rewrite_series_block(
            &block_id,
            json!({
                "source": seam_fixture()["block"]["source"],
                "series": ["US:NVDA"],
                "as_of": "2026-09-10"
            }),
        )
        .await;
    assert_ne!(new_rev, rev);
    let (status, body) = route
        .get(fx.track_id(), &block_id, &format!("?rev={rev}"))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body, json!({ "current_rev": new_rev }));
}

#[tokio::test]
async fn series_route_404_for_missing_or_non_series_block() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let route = Route::new(&fx).await;
    let series_block = fx.write_series_block(seam_fixture()["block"].clone()).await;
    let prose_id = {
        use calm_server::mcp_server::tools::track_report_blocks::TOOL_REPORT_BLOCKS_UPSERT;
        let read = fx
            .read(json!({ "resolve": { series_block.clone(): "none" } }))
            .await;
        let out = crate::mcp_track_report::call_tool(
            &fx.boot,
            TOOL_REPORT_BLOCKS_UPSERT,
            crate::mcp_track_report::planner_identity(&fx.boot),
            json!({ "kind": "prose", "markdown": "words", "if_doc_rev": read["docRev"] }),
        )
        .await
        .expect("prose upsert");
        out["id"].as_str().unwrap().to_string()
    };

    let (status, body) = route.get(fx.track_id(), "b-nope", "?rev=1").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, body) = route.get(fx.track_id(), &prose_id, "?rev=1").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a prose block is not a series block: {body}"
    );
    let (status, body) = route.get("w_missing", &series_block, "?rev=1").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, _) = route.get(fx.track_id(), "b-nope", "?rev=999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(fx.resolver().recorded_outcomes().is_empty());
    assert_eq!(fx.call_count(), 0);
}

#[tokio::test]
async fn series_route_summary_omits_points_and_full_includes_them() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let route = Route::new(&fx).await;
    let block_id = resolved_seam_block(&fx).await;
    let rev = current_rev(&fx, &block_id).await;
    let seam = seam_fixture();

    let (status, summary) = route
        .get(
            fx.track_id(),
            &block_id,
            &format!("?rev={rev}&detail=summary"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{summary}");
    assert_eq!(summary["status"], "ok");
    let entries = summary["series"].as_array().expect("series");
    assert_eq!(entries.len(), 2);
    for entry in entries {
        assert!(
            entry.get("points").is_none(),
            "summary carries no points: {entry}"
        );
        assert!(entry["n"].is_u64(), "summary carries the numbers: {entry}");
    }

    for query in [format!("?rev={rev}"), format!("?rev={rev}&detail=full")] {
        let (status, full) = route.get(fx.track_id(), &block_id, &query).await;
        assert_eq!(status, StatusCode::OK, "{full}");
        let entries = full["series"].as_array().expect("series");
        assert_eq!(entries.len(), 2);
        for (entry, replied) in entries
            .iter()
            .zip(seam["reply"]["series"].as_array().unwrap())
        {
            assert_eq!(entry["points"], replied["points"], "{query}: {entry}");
        }
        let mut stripped = full.clone();
        for entry in stripped["series"].as_array_mut().unwrap() {
            entry.as_object_mut().unwrap().remove("points");
        }
        assert_eq!(stripped, summary);
    }

    let (status, body) = route
        .get(fx.track_id(), &block_id, &format!("?rev={rev}&detail=none"))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(fx.resolver().recorded_outcomes().len(), 1);
    assert_eq!(fx.call_count(), 1);
}

#[tokio::test]
async fn series_route_enqueues_when_no_row() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let route = Route::new(&fx).await;
    let block_id = fx.write_series_block(seam_fixture()["block"].clone()).await;
    let rev = current_rev(&fx, &block_id).await;
    assert!(fx.resolver().recorded_outcomes().is_empty());

    let (status, body) = route
        .get(fx.track_id(), &block_id, &format!("?rev={rev}"))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "pending", "{body}");
    assert!(body.get("reason").is_none(), "queued, not a miss: {body}");
    assert_eq!(body["view"], "line");
    assert_eq!(body["range"], "1M");
    assert_eq!(fx.resolver().recorded_outcomes(), vec![Enqueue::Queued]);
    assert_eq!(fx.call_count(), 0, "the route never calls the plugin");
    assert!(fx.rows().await.is_empty(), "the route never writes");

    fx.reply_structured(seam_fixture()["reply"].clone());
    let outcomes = fx.run_recorded_jobs().await;
    assert_eq!(outcomes.len(), 1);
    let (status, body) = route
        .get(fx.track_id(), &block_id, &format!("?rev={rev}"))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "ok", "{body}");
    assert_eq!(body["pinned"], true);
}

#[tokio::test]
async fn series_route_and_mcp_read_return_the_same_bytes() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let route = Route::new(&fx).await;
    let block_id = resolved_seam_block(&fx).await;
    let rev = current_rev(&fx, &block_id).await;

    for (query, resolve) in [
        (format!("?rev={rev}&detail=summary"), json!({})),
        (
            format!("?rev={rev}"),
            json!({ "resolve": { block_id.clone(): "full" } }),
        ),
    ] {
        let (status, http) = route.get(fx.track_id(), &block_id, &query).await;
        assert_eq!(status, StatusCode::OK, "{http}");
        let read = fx.read(resolve).await;
        let mcp = SeriesFixture::resolved_of(&read, &block_id).clone();
        assert_eq!(http, mcp, "{query}");
        assert_eq!(
            serde_json::to_string(&http).unwrap(),
            serde_json::to_string(&mcp).unwrap(),
            "{query}"
        );
        for key in [
            "status",
            "as_of",
            "resolved_at",
            "pinned",
            "view",
            "field",
            "period",
            "range",
            "series",
        ] {
            assert!(http.get(key).is_some(), "{key} missing from {http}");
        }
    }
    let mut seam_resolved = seam_fixture()["resolved"].clone();
    let (_, http) = route
        .get(
            fx.track_id(),
            &block_id,
            &format!("?rev={rev}&detail=summary"),
        )
        .await;
    let mut http = http;
    http.as_object_mut().unwrap().remove("resolved_at");
    seam_resolved.as_object_mut().unwrap().remove("resolved_at");
    assert_eq!(
        http, seam_resolved,
        "the route serves the seam's resolved shape"
    );
}

#[tokio::test]
async fn series_route_requires_a_session() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let (app, _) = app(&fx);
    let block_id = fx.write_series_block(seam_fixture()["block"].clone()).await;
    let (status, _) = get(
        &app,
        &format!(
            "/api/tracks/{}/report/series/{block_id}?rev=1",
            fx.track_id()
        ),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(fx.resolver().recorded_outcomes().is_empty());
}
