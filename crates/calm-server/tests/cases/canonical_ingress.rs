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

    async fn state(&self) -> AppState {
        let repo: Arc<dyn Repo> = self.repo.clone();
        let events = EventBus::new();
        let roles = calm_server::card_role_cache::CardRoleCache::new();
        let tracks = calm_server::track_area_cache::TrackAreaCache::new();
        self.repo.seed_card_role_cache(&roles).await.unwrap();
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
        state
    }

    async fn application_with_auth(&self, auth: AuthState) -> axum::Router {
        routes::application_router(self.state().await, auth)
    }

    async fn application(&self) -> (axum::Router, String) {
        let app = self
            .application_with_auth(AuthState::new(AuthConfig {
                username: Some("jian".into()),
                password: Some("fixture-password".into()),
                dev_autologin: false,
                display_name: "Jian".into(),
            }))
            .await;
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
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM conversation_messages WHERE card_id=?1")
            .bind(&card)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        count, 2,
        "a retried send cannot leave an extra stored message"
    );
}

#[tokio::test]
async fn actual_session_authors_bind_plain_send_keys_and_recheck_revocation() {
    use calm_server::auth::{SessionAuthority, SessionIdentity};
    let fixture = Fixture::new().await;
    let auth = AuthState::new(AuthConfig {
        username: Some("jian".into()),
        password: Some("fixture-password".into()),
        dev_autologin: false,
        display_name: "Jian".into(),
    });
    let owner_token = auth.sessions.create(
        SessionIdentity::owner(&auth.config),
        SessionAuthority::PasswordLogin,
    );
    let alice_token = auth.sessions.create(
        SessionIdentity {
            user_id: "alice".into(),
            display_name: "Alice".into(),
            role: "member".into(),
        },
        SessionAuthority::PairedDevice,
    );
    let owner = format!("calm-session={owner_token}");
    let alice = format!("calm-session={alice_token}");
    let app = fixture.application_with_auth(auth.clone()).await;
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(fixture.repo.pool())
        .await
        .unwrap();
    calm_server::db::sqlite::area_membership::application_user_register(&mut tx, "alice", "Alice")
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO area_members(area_id,user_id,role,joined_at) VALUES(?1,'alice','member',0)",
    )
    .bind(&fixture.area)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (status, created) = post(&app, &owner, "/api/tracks", "first", fixture.create_body()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let (card, _, _) = fixture.carrier(created["id"].as_str().unwrap()).await;
    let uri = format!("/api/cards/{card}/planner/input");
    let body = json!({"text":"same words", "attachments":[]});
    let (status, original) = post(&app, &owner, &uri, "ordinary", body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{original}");
    let (status, changed) = post(&app, &owner, &uri, "ordinary", json!({"text":"different"})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{changed}");
    assert_eq!(changed["code"], "idempotency_key_reused");
    let (status, foreign) = post(&app, &alice, &uri, "ordinary", body.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{foreign}");
    assert_eq!(foreign["code"], "idempotency_key_reused");
    let (status, own) = post(&app, &alice, &uri, "alice-key", body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{own}");
    assert_ne!(own["entry_id"], original["entry_id"]);
    let authors: Vec<String> = sqlx::query_scalar(
        "SELECT author_user_id FROM conversation_messages WHERE card_id=?1 ORDER BY rowid",
    )
    .bind(&card)
    .fetch_all(fixture.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        authors,
        vec![
            LEGACY_OWNER_USER_ID.to_string(),
            LEGACY_OWNER_USER_ID.to_string(),
            "alice".to_string()
        ]
    );
    sqlx::query("DELETE FROM area_members WHERE area_id=?1 AND user_id='alice'")
        .bind(&fixture.area)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    let (status, denied) = post(&app, &alice, &uri, "alice-key", body.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
    auth.sessions.remove(&owner_token).unwrap();
    let (status, denied) = post(&app, &owner, &uri, "ordinary", body).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{denied}");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM conversation_messages WHERE card_id=?1")
            .bind(&card)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        count, 3,
        "refusals and retries cannot create accepted messages"
    );
}

#[tokio::test]
async fn a_bare_actor_cannot_supply_the_required_first_message_source() {
    let fixture = Fixture::new().await;
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(fixture.state().await);
    let (status, body) = post(&app, "", "/api/tracks", "first", fixture.create_body()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM tracks WHERE area_id=?1")
        .bind(&fixture.area)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "Actor data cannot authorize or allocate Human input"
    );
}

#[tokio::test]
async fn unsupported_fresh_shapes_and_historical_keys_never_fall_back_to_v1() {
    let fixture = Fixture::new().await;
    let (app, cookie) = fixture.application().await;
    let mut attached = fixture.create_body();
    attached["cwd"] = json!(fixture.tmp.path().display().to_string());
    let (status, body) = post(&app, &cookie, "/api/tracks", "unsupported", attached).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, created) =
        post(&app, &cookie, "/api/tracks", "first", fixture.create_body()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let (card, runtime, before) = fixture.carrier(created["id"].as_str().unwrap()).await;
    let uri = format!("/api/cards/{card}/planner/input");
    for body in [
        json!({"text":"edit","replaces_turn":"native-turn"}),
        json!({"text":"image","attachments":["00000000-0000-4000-8000-000000000000.png"]}),
    ] {
        let (status, answer) = post(&app, &cookie, &uri, "unsupported-send", body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    }
    sqlx::query(
        "INSERT INTO planner_input_idempotency
         (card_id,idempotency_key,payload_hash,worker_session_id,entry_id,created_at_ms)
         VALUES(?1,'historical','v1:opaque-original',?2,'legacy-entry',0)",
    )
    .bind(&card)
    .bind(&runtime)
    .execute(fixture.repo.pool())
    .await
    .unwrap();
    let (status, body) = post(
        &app,
        &cookie,
        &uri,
        "historical",
        json!({"text":"same intent"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "idempotency_key_reused");
    assert_eq!(
        fixture.carrier(created["id"].as_str().unwrap()).await.2,
        before
    );
    let hash: String = sqlx::query_scalar(
        "SELECT payload_hash FROM planner_input_idempotency
         WHERE card_id=?1 AND idempotency_key='historical'",
    )
    .bind(&card)
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        hash, "v1:opaque-original",
        "unproven historical evidence is unchanged"
    );
}

#[tokio::test]
async fn canonical_send_replay_does_not_restore_its_original_runtime() {
    let fixture = Fixture::new().await;
    let (app, cookie) = fixture.application().await;
    let (status, created) =
        post(&app, &cookie, "/api/tracks", "first", fixture.create_body()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let (card, original, _) = fixture.carrier(created["id"].as_str().unwrap()).await;
    let uri = format!("/api/cards/{card}/planner/input");
    let body = json!({"text":"original send"});
    let (status, ack) = post(&app, &cookie, &uri, "ordinary", body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{ack}");
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(fixture.repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET state='failed' WHERE id=?1")
        .bind(&original)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET session_id=NULL WHERE id=?1")
        .bind(&card)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (status, replay) = post(&app, &cookie, &uri, "ordinary", body).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(
        replay, ack,
        "the original ACK is not a request to revive its runtime"
    );
    let state: String = sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id=?1")
        .bind(&original)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    assert_eq!(state, "failed");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM conversation_messages WHERE card_id=?1")
            .bind(&card)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(count, 2);
}

async fn get(app: &axum::Router, cookie: &str, uri: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn conversation_reader_shows_saved_human_before_native_and_after_reopen() {
    let mut fixture = Fixture::new().await;
    let (app, cookie) = fixture.application().await;
    let (status, track) = post(
        &app,
        &cookie,
        "/api/tracks",
        "read-first",
        fixture.create_body(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{track}");
    let (card, _, _) = fixture.carrier(track["id"].as_str().unwrap()).await;
    let uri =
        format!("/api/cards/{card}/harness/items?contract=conversation_v1&direction=desc&limit=1");
    let (status, page) = get(&app, &cookie, &uri).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["contract"], "conversation_v1", "{page}");
    assert_eq!(page["source"], "presentation");
    assert_eq!(page["items"][0]["source"]["kind"], "human");
    assert_eq!(
        page["items"][0]["source"]["author"]["user_id"],
        LEGACY_OWNER_USER_ID
    );
    assert_eq!(
        page["items"][0]["source"]["text"],
        "Keep the original accepted input"
    );
    assert!(page["items"][0]["source"].get("thread_id").is_none());
    let (_, legacy) = get(&app, &cookie, &format!("/api/cards/{card}/harness/items")).await;
    assert_eq!(
        legacy["code"], "conflict",
        "canonical cannot silently read raw items"
    );
    drop(app);
    fixture.repo.pool().close().await;
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.database_url).await.unwrap());
    let (app, cookie) = fixture.application().await;
    assert_eq!(get(&app, &cookie, &uri).await, (StatusCode::OK, page));
}

#[tokio::test]
async fn conversation_run_reads_durable_frame_without_live_handle() {
    let fixture = Fixture::new().await;
    let (app, cookie) = fixture.application().await;
    let (status, track) = post(
        &app,
        &cookie,
        "/api/tracks",
        "run-first",
        fixture.create_body(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{track}");
    let (card, worker, _) = fixture.carrier(track["id"].as_str().unwrap()).await;
    let (status, run) = get(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/run?contract=conversation_v1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(run["input_contract"], "canonical", "{run}");
    assert_eq!(run["worker_session_id"], worker);
    assert_eq!(run["phase"], "idle");
    assert_eq!(run["pending_messages"].as_array().unwrap().len(), 1);
    assert!(run["pending_messages"][0].get("text").is_none());
}

#[tokio::test]
async fn conversation_reader_rejects_cross_card_cursor() {
    let fixture = Fixture::new().await;
    let (app, cookie) = fixture.application().await;
    let (status, track) = post(
        &app,
        &cookie,
        "/api/tracks",
        "cursor-first",
        fixture.create_body(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{track}");
    let (card, _, _) = fixture.carrier(track["id"].as_str().unwrap()).await;
    let cursor = "%7B%22contract%22:%22conversation_v1%22,%22card_id%22:%22other%22,%22source%22:%22presentation%22,%22position%22:1%7D";
    let (status, body) = get(
        &app,
        &cookie,
        &format!("/api/cards/{card}/harness/items?contract=conversation_v1&cursor={cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[path = "canonical_presentation.rs"]
mod presentation;
