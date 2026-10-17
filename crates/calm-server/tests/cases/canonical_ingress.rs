//! Acceptance must survive native unavailability through the production HTTP routes.
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{AuthConfig, AuthState};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use calm_types::conversation::ConversationInputCheckpoint;
use calm_types::organization::LEGACY_OWNER_USER_ID;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Fixture {
    repo: Arc<SqlxRepo>,
    tmp: TempDir,
    database_url: String,
    area: String,
}

impl Fixture {
    async fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let database_url = format!(
            "sqlite://{}?mode=rwc",
            tmp.path().join("accepted-inputs.db").display()
        );
        let repo = Arc::new(SqlxRepo::open(&database_url).await.unwrap());
        let area = repo
            .area_create(
                LEGACY_OWNER_USER_ID,
                calm_server::model::NewArea {
                    name: "Canonical HTTP acceptance".into(),
                    color: "#123456".into(),
                    sort: None,
                },
            )
            .await
            .unwrap();
        Self {
            repo,
            tmp,
            database_url,
            area: area.id.to_string(),
        }
    }

    async fn application(&self) -> (axum::Router, String) {
        let repo: Arc<dyn Repo> = self.repo.clone();
        let events = EventBus::new();
        let roles = calm_server::card_role_cache::CardRoleCache::new();
        let tracks = calm_server::track_area_cache::TrackAreaCache::new();
        self.repo.seed_track_area_cache(&tracks).await.unwrap();
        let plugin = Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            self.tmp.path().join("plugins-data"),
            Vec::new(),
            events.clone(),
            WriteContext::new(roles.clone(), tracks.clone()),
        ));
        // Both clients are genuinely unavailable. No fake-running registry or
        // replacement acceptance implementation makes these requests succeed.
        let state = AppState::from_parts(
            repo,
            events,
            Arc::new(DaemonClient::new_stub()),
            plugin,
            Arc::new(CodexClient::new_stub()),
            Some(roles),
            Some(tracks),
        )
        .with_workspace_root(self.tmp.path().join("workspaces"));
        let app = routes::application_router(
            state,
            AuthState::new(AuthConfig {
                username: Some("jian".into()),
                password: Some("fixture-password".into()),
                dev_autologin: false,
                display_name: "Jian".into(),
            }),
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/login")
                    .extension(ConnectInfo("127.0.0.1:4444".parse::<SocketAddr>().unwrap()))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({"username":"jian","password":"fixture-password"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        (app, cookie)
    }

    fn create_body(&self) -> Value {
        json!({
            "area_id":self.area,
            "title":"",
            "planner_provider":"codex",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]},
            "first_message":"Keep the original accepted input"
        })
    }

    async fn carrier(&self, track: &str) -> (String, String, Value) {
        let (card, worker, snapshot, native_thread): (String, String, String, Option<String>) =
            sqlx::query_as(
                "SELECT c.id,ws.id,ws.handle_state_json,ws.thread_id
                 FROM cards c JOIN worker_sessions ws ON c.session_id=ws.id
                 WHERE c.track_id=?1 AND c.role='planner'",
            )
            .bind(track)
            .fetch_one(self.repo.pool())
            .await
            .unwrap();
        assert_eq!(
            native_thread, None,
            "acceptance cannot invent a native thread"
        );
        let snapshot: Value = serde_json::from_str(&snapshot).unwrap();
        assert_eq!(snapshot["schema_version"], 2);
        assert!(snapshot.get("pending_queue").is_none());
        assert!(snapshot.get("issued_input_segments").is_none());
        (card, worker, snapshot)
    }
}

async fn post(
    app: &axum::Router,
    cookie: &str,
    uri: &str,
    key: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .header("Idempotency-Key", key)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn unavailable_native_create_accepts_once_and_rebuilt_application_replays_original() {
    let mut fixture = Fixture::new().await;
    let (app, cookie) = fixture.application().await;
    let (status, first) = post(&app, &cookie, "/api/tracks", "first", fixture.create_body()).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let track = first["id"]
        .as_str()
        .expect("existing Track response contract");
    let before = fixture.carrier(track).await;
    let checkpoint: ConversationInputCheckpoint =
        serde_json::from_value(before.2["canonical_inputs"].clone()).unwrap();
    assert_eq!(checkpoint.pending.len(), 1);
    assert!(checkpoint.active.is_none());
    assert!(checkpoint.attempts.is_empty());
    let author: String =
        sqlx::query_scalar("SELECT author_user_id FROM conversation_messages WHERE card_id=?1")
            .bind(&before.0)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(author, LEGACY_OWNER_USER_ID);
    drop(app);
    fixture.repo.pool().close().await;
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.database_url).await.unwrap());

    // Open the actual database again, reconstruct production AppState/registry
    // and log in through the issuer. This checks persisted acceptance, not
    // automatic native execution recovery after a process crash.
    let (rebuilt, cookie) = fixture.application().await;
    let (status, replay) = post(
        &rebuilt,
        &cookie,
        "/api/tracks",
        "first",
        fixture.create_body(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{replay}");
    assert_eq!(replay["id"], first["id"]);
    assert_eq!(fixture.carrier(track).await, before);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM conversation_messages WHERE card_id=?1")
            .bind(&before.0)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn unavailable_native_plain_send_persists_before_wake_and_replays_without_second_message() {
    let fixture = Fixture::new().await;
    let (app, cookie) = fixture.application().await;
    let (status, created) =
        post(&app, &cookie, "/api/tracks", "first", fixture.create_body()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let track = created["id"].as_str().unwrap();
    let (card, worker, _) = fixture.carrier(track).await;
    let uri = format!("/api/cards/{card}/planner/input");
    let body = json!({"text":"A second ordinary message","attachments":[]});
    let (status, first) = post(&app, &cookie, &uri, "second", body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["worker_session_id"], worker);
    assert!(
        first["entry_id"].is_string(),
        "accepted input must be addressable"
    );
    let before = fixture.carrier(track).await;
    let checkpoint: ConversationInputCheckpoint =
        serde_json::from_value(before.2["canonical_inputs"].clone()).unwrap();
    assert_eq!(checkpoint.pending.len(), 2);
    assert!(checkpoint.active.is_none());
    assert!(checkpoint.attempts.is_empty());
    let (status, replay) = post(&app, &cookie, &uri, "second", body).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay, first);
    assert_eq!(fixture.carrier(track).await, before);
}
