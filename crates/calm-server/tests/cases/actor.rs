//! `X-Calm-Actor` middleware tests, asserted against `events.actor` directly.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use calm_server::actor::actor_middleware;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, overlay_upsert_tx};
use calm_server::db::write_with_event_typed;
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::ActorId;
use calm_server::model::{NewArea, NewOverlay, NewTrack};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use tower::ServiceExt;

/// Router with the actor middleware wired in, matching `main.rs`.
async fn boot() -> (axum::Router, Arc<SqlxRepo>, AppState) {
    let concrete = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo: Arc<dyn Repo> = concrete.clone();
    let events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo,
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            events,
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );
    let app = axum::Router::new()
        .merge(routes::router())
        .layer(axum::middleware::from_fn(actor_middleware))
        .with_state(state.clone());
    (app, concrete, state)
}

/// `POST /api/areas` and return the status plus (if 2xx) the recorded actor of the resulting event.
async fn post_area_and_read_actor(
    app: axum::Router,
    repo: &SqlxRepo,
    header: Option<&str>,
) -> (StatusCode, Option<String>) {
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/areas")
        .header("content-type", "application/json");
    if let Some(h) = header {
        req = req.header("X-Calm-Actor", h);
    }
    let body = serde_json::json!({ "name": "c", "color": "#000" }).to_string();
    let resp = app
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    if !status.is_success() {
        return (status, None);
    }

    let row: (String, String) =
        sqlx::query_as("SELECT kind, actor FROM events ORDER BY id DESC LIMIT 1")
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(
        row.0, "area.updated",
        "expected area.updated, got {}",
        row.0
    );
    (status, Some(row.1))
}

fn parse_actor_json(s: &str) -> serde_json::Value {
    serde_json::from_str(s).expect("events.actor is JSON-serialized ActorId")
}

#[tokio::test]
async fn missing_header_defaults_to_user_actor() {
    let (app, repo, _state) = boot().await;
    let (status, actor) = post_area_and_read_actor(app, &repo, None).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        parse_actor_json(actor.as_deref().unwrap()),
        serde_json::json!({"kind": "User"})
    );
}

#[tokio::test]
async fn ai_codex_header_rejected_without_card_context() {
    // `ai:codex` on REST maps to `AiCodex(CardId(""))`, which the `enforce_role` empty-CardId guard refuses.
    let (app, _repo, _state) = boot().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/areas")
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "ai:codex")
                .body(Body::from(
                    serde_json::json!({ "name": "c", "color": "#000" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn valid_ai_actor_with_dashes_recorded() {
    let (app, repo, _state) = boot().await;
    let (status, actor) = post_area_and_read_actor(app, &repo, Some("ai:claude-3-5")).await;
    assert_eq!(status, StatusCode::CREATED);
    // Non-`codex` AI ids collapse to the defensive `User` fallback.
    assert_eq!(
        parse_actor_json(actor.as_deref().unwrap()),
        serde_json::json!({"kind": "User"})
    );
}

#[tokio::test]
async fn kernel_actor_rejected_from_header() {
    let (app, repo, _state) = boot().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/areas")
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "kernel")
                .body(Body::from(
                    serde_json::json!({ "name": "c", "color": "#000" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(row.0, 0, "rejected header must not produce an event row");
}

#[tokio::test]
async fn plugin_actor_rejected_from_header() {
    let (app, repo, _state) = boot().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/areas")
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "plugin:hello-world")
                .body(Body::from(
                    serde_json::json!({ "name": "c", "color": "#000" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(row.0, 0);
}

#[tokio::test]
async fn empty_ai_id_rejected() {
    let (app, _repo, _state) = boot().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/areas")
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "ai:")
                .body(Body::from(
                    serde_json::json!({ "name": "c", "color": "#000" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = to_bytes(resp.into_body(), 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["code"], "bad_request");
}

#[tokio::test]
async fn uppercase_ai_id_rejected() {
    let (app, _repo, _state) = boot().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/areas")
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "ai:UPPER")
                .body(Body::from(
                    serde_json::json!({ "name": "c", "color": "#000" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// The plugin-callback dispatcher writes `plugin:<id>` directly and does not go through the REST middleware.

#[tokio::test]
async fn plugin_callback_path_writes_plugin_actor_regardless_of_middleware() {
    let (_app, repo, state) = boot().await;

    let area = repo
        .area_create(NewArea {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "w".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    // Exactly the write `plugin_host::callbacks::overlay_set` performs after the perm check.
    let plugin_id = "hello-world";
    let actor = ActorId::Plugin(plugin_id.to_string());
    let new_overlay = NewOverlay {
        plugin_id: plugin_id.to_string(),
        entity_kind: "track".into(),
        entity_id: track.id.to_string(),
        kind: "status".into(),
        payload: serde_json::json!({ "state": "Idle" }),
    };
    let (overlay, event_id) = write_with_event_typed(
        state.repo.as_ref(),
        actor,
        EventScope::Track {
            track: track.id.clone(),
            area: area.id.clone(),
        },
        None,
        &state.events,
        &calm_server::state::WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::track_area_cache::TrackAreaCache::new(),
        ),
        move |tx| {
            Box::pin(async move {
                let o = overlay_upsert_tx(tx, new_overlay).await?;
                Ok((o.clone(), Event::OverlaySet(o)))
            })
        },
    )
    .await
    .expect("plugin overlay write");
    assert_eq!(overlay.plugin_id, plugin_id);

    let row: (String, String) = sqlx::query_as("SELECT kind, actor FROM events WHERE id = ?1")
        .bind(event_id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "overlay.set");
    let actor_json: serde_json::Value = serde_json::from_str(&row.1).unwrap();
    assert_eq!(
        actor_json,
        serde_json::json!({"kind": "Plugin", "id": "hello-world"}),
        "plugin-callback path must stamp Plugin(<id>) even when REST middleware would reject it"
    );
}

#[tokio::test]
async fn create_card_stamps_full_scope_chain() {
    let (app, repo, _state) = boot().await;

    let area = repo
        .area_create(NewArea {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "w".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let body = serde_json::json!({
        "kind": "plugin:test:demo",
        "payload": {}
    })
    .to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{}/cards", track.id))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp_body = to_bytes(resp.into_body(), 8192).await.unwrap();
    let card_json: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
    let card_id = card_json["id"].as_str().expect("card id").to_string();

    let row: (String, Option<String>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT scope_kind, scope_area, scope_track, scope_card
         FROM events ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(row.0, "card", "scope_kind == 'card' for card.added");
    assert_eq!(
        row.1.as_deref(),
        Some(area.id.as_str()),
        "scope_area populated"
    );
    assert_eq!(
        row.2.as_deref(),
        Some(track.id.as_str()),
        "scope_track populated"
    );
    assert_eq!(
        row.3.as_deref(),
        Some(card_id.as_str()),
        "scope_card populated with the freshly-minted card id"
    );
}
