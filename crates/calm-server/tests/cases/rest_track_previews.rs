//! #1780 S2b: `GET /api/tracks/{id}/previews` through the production router tree, with a real
//! TCP listener behind the `live` probe.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{self, AuthConfig, AuthState, SessionAuthority};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::ids::{AreaId, TrackId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::model::{NewArea, NewTrack};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::preview::{PreviewPorts, PreviewRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    session: String,
    registry: Arc<PreviewRegistry>,
}

async fn create_track(repo: &SqlxRepo, area_id: &AreaId, title: &str) -> TrackId {
    repo.track_create(NewTrack {
        template_input: None,
        area_id: area_id.clone(),
        title: title.into(),
        sort: None,
        cwd: String::new(),
        template_id: None,
        plugin_scope: None,
        attach_folder: false,
        theme: calm_server::routes::theme::RequestTheme::default_dark(),
    })
    .await
    .unwrap()
    .id
}

/// Tracks `a` and `b` with a preview pool of 4050-4053 on the route's own `AppContext`.
async fn boot() -> (Boot, TrackId, TrackId) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "previews".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let a = create_track(&repo, &area.id, "a").await;
    let b = create_track(&repo, &area.id, "b").await;
    let events = EventBus::new();
    let write = WriteContext::new(
        calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::track_area_cache::TrackAreaCache::new(),
    );
    let registry = Arc::new(PreviewRegistry::new(
        PreviewPorts::parse("4050-4053").unwrap(),
        4040,
    ));
    let ctx = AppContext::new(
        repo.clone(),
        events.clone(),
        write.clone(),
        None,
        Arc::new(tokio::sync::OnceCell::new()),
        Arc::new(tokio::sync::OnceCell::new()),
        std::env::temp_dir().join("calm-rest-track-previews-gate-logs"),
        1,
    )
    .with_preview(registry.clone());
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-rest-previews"),
            Vec::new(),
            events,
            write,
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    )
    .with_mcp_context(ctx);
    let auth_state = AuthState::new(AuthConfig {
        username: Some("alice".into()),
        password: Some("hunter2".into()),
        dev_autologin: false,
        display_name: "alice".into(),
    });
    let session = format!(
        "{}={}",
        auth::SESSION_COOKIE,
        auth_state.sessions.create(SessionAuthority::PasswordLogin)
    );
    let protected_rest = routes::protected_router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            auth_state.clone(),
            auth::require_session,
        ));
    let app = axum::Router::new()
        .merge(protected_rest)
        .merge(routes::public_router())
        .with_state(state)
        .merge(auth::router().with_state(auth_state));
    let boot = Boot {
        app,
        session,
        registry,
    };
    (boot, a, b)
}

impl Boot {
    async fn get(&self, track: &str, cookie: Option<&str>) -> (StatusCode, Value) {
        let mut request = Request::builder().uri(format!("/api/tracks/{track}/previews"));
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        let resp = self
            .app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
}

/// A loopback port nothing listens on: bound, read, then released.
async fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

#[tokio::test]
async fn previews_route_lists_own_track_with_a_real_live_probe() {
    let (boot, a, b) = boot().await;
    let target = closed_port().await;
    assert_eq!(boot.registry.register(&a, "fe", "Web FE", target), Ok(4050));
    assert_eq!(boot.registry.register(&b, "fe", "Other", 5999), Ok(4051));
    let session = Some(boot.session.as_str());

    let (status, body) = boot.get(a.as_str(), session).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({ "previews": [{ "key": "fe", "title": "Web FE", "port": 4050, "live": false }] })
    );

    let listener = TcpListener::bind(("127.0.0.1", target)).await.unwrap();
    let (_, body) = boot.get(a.as_str(), session).await;
    assert_eq!(body["previews"][0]["live"], json!(true), "{body}");
    drop(listener);

    assert_eq!(boot.registry.unregister(&a, "fe"), Some(4050));
    let (_, body) = boot.get(a.as_str(), session).await;
    assert_eq!(body, json!({ "previews": [] }));
}

#[tokio::test]
async fn previews_route_requires_a_session_and_a_known_track() {
    let (boot, a, _) = boot().await;
    let (status, _) = boot.get(a.as_str(), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = boot.get("no-such-track", Some(&boot.session)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
