//! #1829 S3 — Dismiss: `POST /api/tracks/{id}/activity/dismissals` stores an item key and the
//! `kernel/track/activity` projector drops it. Rows 4b, 18, 20 and 21 of the design's producer ×
//! state matrix (§7); every dismissal goes through the production route. Row 4b resolves its ratify
//! request; rows 18, 20 and 21 request one and never resolve it (see the fixture note in
//! `track_notifications.rs`).

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::actor::Actor;
use calm_server::event::EventBus;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_activity::{ActivityWake, Attention};
use serde_json::json;
use tower::ServiceExt;

use super::track_activity_fixture::{Fx, fx};
use super::track_notifications::{
    asks, codex_planner, planner_down, request_ratify, resolve_ratify, running_loop, turn,
};

/// The production router over the fixture's repo, bus and caches; the routes wake `wake`.
pub(crate) fn app(f: &Fx, wake: ActivityWake) -> axum::Router {
    let plugin = PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        f.repo_dyn.clone(),
        PathBuf::new(),
        std::env::temp_dir().join("calm-plugins-data-activity-dismissals"),
        Vec::new(),
        EventBus::new(),
        f.write.clone(),
    );
    let state = AppState::from_parts(
        f.repo_dyn.clone(),
        f.events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(plugin),
        Arc::new(CodexClient::new_stub()),
        Some(f.role_cache.clone()),
        Some(f.area_cache.clone()),
    )
    .with_activity_wake(wake);
    calm_server::routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state)
}

async fn dismiss(app: &axum::Router, track: &str, key: &str, actor: &str) -> StatusCode {
    let body = serde_json::to_vec(&json!({ "key": key })).unwrap();
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{track}/activity/dismissals"))
                .header("content-type", "application/json")
                .header(Actor::HEADER, actor)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

/// `(item_key, dismissed_at_ms)` of every stored dismissal, by key.
async fn dismissals(f: &Fx) -> Vec<(String, i64)> {
    sqlx::query_as(
        "SELECT item_key, dismissed_at_ms FROM activity_dismissals ORDER BY track_id, item_key",
    )
    .fetch_all(&f.pool)
    .await
    .unwrap()
}

// Row 4b.
#[tokio::test]
async fn second_ratify_request_after_dismiss_relights() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    let app = app(&f, ActivityWake::detached());
    request_ratify(&f, &p, "First question?").await;
    let first = f.recompute(&p.track).await.items[0].key.clone();
    assert_eq!(
        dismiss(&app, &p.track, &first, "user").await,
        StatusCode::NO_CONTENT
    );
    let a = f.recompute(&p.track).await;
    assert!(a.items.is_empty(), "the dismissed ask is gone: {a:?}");
    resolve_ratify(&f, &p).await;
    request_ratify(&f, &p, "Second question?").await;
    let a = f.recompute(&p.track).await;
    assert_eq!(a.items.len(), 1, "a new ratify request is a new key: {a:?}");
    assert_eq!(a.items[0].text, "Second question?");
    assert_ne!(a.items[0].key, first);
    assert_eq!(a.attention, Attention::Input);
}

// Row 18.
#[tokio::test]
async fn dismiss_hides_only_that_key() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    let app = app(&f, ActivityWake::detached());
    request_ratify(&f, &p, "Which region?").await;
    turn(&f, &p, "turn-1", "failed", Some("403 Forbidden")).await;
    let a = f.recompute(&p.track).await;
    assert_eq!(a.items.len(), 2, "{a:?}");
    let ask = asks(&a)[0].key.clone();
    let down = planner_down(&a)[0].key.clone();
    assert_eq!(
        dismiss(&app, &p.track, &ask, "user").await,
        StatusCode::NO_CONTENT
    );
    let a = f.recompute(&p.track).await;
    assert_eq!(a.items.len(), 1, "only the ask went: {a:?}");
    assert_eq!(a.items[0].key, down);
    assert_eq!(a.attention, Attention::Failed);
}

// Row 20.
#[tokio::test]
async fn dismissal_wakes_the_projector() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    request_ratify(&f, &p, "Which region?").await;
    let (loop_task, wake) = running_loop(&f, &p.track, |a| asks(a).len() == 1).await;
    let app = app(&f, wake);
    let key = f.stored(&p.track).await.unwrap().items[0].key.clone();
    assert_eq!(
        dismiss(&app, &p.track, &key, "user").await,
        StatusCode::NO_CONTENT
    );
    let a = f
        .await_stored(&p.track, "the item gone by the dismissal's wake-up", |a| {
            a.items.is_empty()
        })
        .await;
    assert_eq!(a.attention, Attention::None);
    loop_task.abort();
}

// Row 21, the actor.
#[tokio::test]
async fn dismiss_route_is_user_only() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    let app = app(&f, ActivityWake::detached());
    request_ratify(&f, &p, "Which region?").await;
    let key = f.recompute(&p.track).await.items[0].key.clone();
    for actor in ["ai:codex", "ai:planner-1"] {
        assert_eq!(
            dismiss(&app, &p.track, &key, actor).await,
            StatusCode::FORBIDDEN,
            "{actor}"
        );
    }
    assert!(dismissals(&f).await.is_empty(), "no row");
    assert_eq!(f.recompute(&p.track).await.items.len(), 1);
}

// Row 21, the key.
#[tokio::test]
async fn dismiss_route_rejects_a_bad_key() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    let app = app(&f, ActivityWake::detached());
    for key in [
        "",
        "ask:ratify:",
        "ask:ratify:abc",
        "ask:ratify:-1",
        "ask:ratify:+1",
        "ask:ratify:1 ",
        "ask:lifecycle:1",
        "ask:1",
        "planner_down",
        "planner_down:99999999999999999999",
        "task:1",
    ] {
        assert_eq!(
            dismiss(&app, &p.track, key, "user").await,
            StatusCode::BAD_REQUEST,
            "{key:?}"
        );
    }
    assert!(dismissals(&f).await.is_empty(), "no row");
}

#[tokio::test]
async fn dismiss_route_is_idempotent() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    let app = app(&f, ActivityWake::detached());
    for key in ["ask:notify:7", "planner_down:8"] {
        assert_eq!(
            dismiss(&app, &p.track, key, "user").await,
            StatusCode::NO_CONTENT
        );
    }
    let first = dismissals(&f).await;
    tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    assert_eq!(
        dismiss(&app, &p.track, "ask:notify:7", "user").await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(dismissals(&f).await, first, "one row, first time kept");
    assert_eq!(
        dismiss(&app, "no-such-track", "ask:notify:7", "user").await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(dismissals(&f).await, first);
}
