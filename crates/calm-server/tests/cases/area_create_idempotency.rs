//! Retry Area creation at the production REST boundary.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    repo: Arc<SqlxRepo>,
    auth: calm_server::auth::AuthState,
    owner_cookie: String,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.expect("open repo"));
    let erased: Arc<dyn Repo> = repo.clone();
    let events = EventBus::new();
    let roles = calm_server::card_role_cache::CardRoleCache::new();
    let tracks = calm_server::track_area_cache::TrackAreaCache::new();
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        erased.clone(),
        PathBuf::new(),
        tmp.path().join("plugins-data"),
        Vec::new(),
        events.clone(),
        WriteContext::new(roles.clone(), tracks.clone()),
    ));
    let state = AppState::from_parts(
        erased,
        events,
        Arc::new(DaemonClient::new_stub()),
        plugin,
        Arc::new(CodexClient::new_stub()),
        Some(roles),
        Some(tracks),
    )
    .with_workspace_root(tmp.path().join("workspaces"));
    let auth = calm_server::auth::AuthState::new(calm_server::auth::AuthConfig {
        username: Some("owner".into()),
        password: Some("fixture-password".into()),
        dev_autologin: false,
        display_name: "Owner".into(),
    });
    let owner_token = auth.sessions.create(
        calm_server::auth::SessionIdentity::owner(&auth.config),
        calm_server::auth::SessionAuthority::PasswordLogin,
    );
    let owner_cookie = format!("{}={owner_token}", calm_server::auth::SESSION_COOKIE);
    let app = routes::application_router(state, auth.clone());
    Boot {
        app,
        repo,
        auth,
        owner_cookie,
        _tmp: tmp,
    }
}

async fn post(boot: &Boot, key: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/api/areas")
        .header("cookie", &boot.owner_cookie)
        .header("content-type", "application/json");
    if let Some(key) = key {
        request = request.header("Idempotency-Key", key);
    }
    let response = boot
        .app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn area_create_idempotency_repeated_request_returns_one_area_and_event() {
    let boot = boot().await;
    let body = json!({"name":"Retry", "color":"#123456"});
    let (first_status, first) = post(&boot, Some("repeat"), body.clone()).await;
    let (second_status, second) = post(&boot, Some("repeat"), body).await;
    assert_eq!(first_status, StatusCode::CREATED);
    assert_eq!(second_status, StatusCode::CREATED);
    assert_eq!(first["id"], second["id"]);
    assert_eq!(boot.repo.areas_list_user_visible().await.unwrap().len(), 1);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE kind = 'area.updated'")
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn area_create_idempotency_concurrent_requests_commit_once() {
    let boot = boot().await;
    let body = json!({"name":"Concurrent", "color":"#123456"});
    let requests = (0..8).map(|_| post(&boot, Some("concurrent"), body.clone()));
    let responses = futures::future::join_all(requests).await;
    let id = &responses[0].1["id"];
    for (status, area) in &responses {
        assert_eq!(*status, StatusCode::CREATED, "{area}");
        assert_eq!(&area["id"], id);
    }
    assert_eq!(boot.repo.areas_list_user_visible().await.unwrap().len(), 1);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE kind = 'area.updated'")
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn area_create_idempotency_binds_every_input_but_allows_independent_same_names() {
    let boot = boot().await;
    let original = json!({"name":"Same name", "color":"#123456"});
    let (_, first) = post(&boot, Some("shape"), original.clone()).await;
    for (field, value) in [
        ("name", json!("Changed")),
        ("color", json!("#abcdef")),
        ("sort", json!(7)),
        ("default_template_id", json!("small-change")),
        ("default_cwd", json!("/missing/path")),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        let (status, body) = post(&boot, Some("shape"), changed).await;
        assert_eq!(status, StatusCode::CONFLICT, "{field}: {body}");
        assert_eq!(body["code"], "idempotency_key_reused", "{field}: {body}");
    }
    assert_eq!(boot.repo.areas_list_user_visible().await.unwrap().len(), 1);
    let explicit_nulls = json!({"name":"Same name", "color":"#123456", "sort":null,
        "default_template_id":null, "default_cwd":null, "kind":"system"});
    let (status, replay) = post(&boot, Some("shape"), explicit_nulls).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(replay["id"], first["id"]);
    for key in [Some("independent"), None, None] {
        let (status, area) = post(&boot, key, original.clone()).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_ne!(area["id"], first["id"]);
    }
    assert_eq!(boot.repo.areas_list_user_visible().await.unwrap().len(), 4);
}

#[tokio::test]
async fn area_create_idempotency_deleted_area_and_binding_tampering_fail_closed() {
    let boot = boot().await;
    let body = json!({"name":"Deleted", "color":"#123456"});
    let (_, first) = post(&boot, Some("deleted"), body.clone()).await;
    let response = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .header("cookie", &boot.owner_cookie)
                .uri(format!("/api/areas/{}", first["id"].as_str().unwrap()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let (status, error) = post(&boot, Some("deleted"), body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(
        error["code"], "idempotency_key_exhausted",
        "a deleted Area spends its key, as a deleted Track does: {error}"
    );
    assert!(
        boot.repo
            .areas_list_user_visible()
            .await
            .unwrap()
            .is_empty()
    );
    for query in [
        "DELETE FROM area_create_idempotency",
        "UPDATE area_create_idempotency SET area_id = 'other'",
    ] {
        assert!(sqlx::query(query).execute(boot.repo.pool()).await.is_err());
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM area_create_idempotency")
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn area_create_idempotency_event_failure_rolls_back_area_and_binding() {
    let boot = boot().await;
    sqlx::query("CREATE TRIGGER test_reject_area_event BEFORE INSERT ON events WHEN NEW.kind = 'area.updated' BEGIN SELECT RAISE(ABORT, 'test event write failed'); END;")
        .execute(boot.repo.pool()).await.unwrap();
    let body = json!({"name":"Atomic", "color":"#123456", "default_template_id":"small-change"});
    let (status, _) = post(&boot, Some("atomic"), body.clone()).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        boot.repo
            .areas_list_user_visible()
            .await
            .unwrap()
            .is_empty()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM area_create_idempotency")
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM area_members")
            .fetch_one(boot.repo.pool())
            .await
            .unwrap(),
        0
    );
    sqlx::query("DROP TRIGGER test_reject_area_event")
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let (status, created) = post(&boot, Some("atomic"), body.clone()).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["default_template_id"], "small-change");
    let (_, replay) = post(&boot, Some("atomic"), body).await;
    assert_eq!(created["id"], replay["id"]);
}

#[tokio::test]
async fn area_create_idempotency_replays_current_row_after_original_folder_is_removed() {
    let boot = boot().await;
    let dir = boot._tmp.path().join("worktree");
    std::fs::create_dir(&dir).unwrap();
    let output = std::process::Command::new("git")
        .arg("init")
        .arg(&dir)
        .output()
        .unwrap();
    assert!(output.status.success());
    let body = json!({"name":"Folder", "color":"#123456", "default_cwd":dir});
    let (status, first) = post(&boot, Some("folder"), body.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let updated = boot
        .repo
        .area_update(
            first["id"].as_str().unwrap(),
            calm_server::model::AreaPatch {
                name: Some("Renamed".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    let (status, replay) = post(&boot, Some("folder"), body).await;
    assert_eq!(status, StatusCode::CREATED, "{replay}");
    assert_eq!(replay["id"], first["id"]);
    assert_eq!(replay["name"], "Renamed");
    assert_eq!(replay["updated_at"], updated.updated_at);
}

#[tokio::test]
async fn area_create_idempotency_malformed_key_and_validation_leave_no_binding() {
    let boot = boot().await;
    let body = json!({"name":"Invalid", "color":"#123456"});
    for key in ["  ".to_string(), "k".repeat(129)] {
        let (status, error) = post(&boot, Some(&key), body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
        assert_eq!(error["code"], "idempotency_key_invalid", "{error}");
    }
    let mut invalid = body.clone();
    invalid["default_template_id"] = json!("missing-template");
    let (status, _) = post(&boot, Some("valid"), invalid).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM area_create_idempotency")
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    let (status, _) = post(&boot, Some("valid"), body).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn area_create_idempotency_version_advertises_safe_retry_capability() {
    let boot = boot().await;
    let response = boot
        .app
        .oneshot(
            Request::builder()
                .uri("/api/version")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["areaCreateIdempotency"], true);
}

/// The binding's primary key is the backstop. A real race cannot reach it: the replay check and the
/// bind share one `BEGIN IMMEDIATE` transaction. So a trigger writes the conflicting binding inside
/// that transaction, after the replay check missed and before the bind, which is the state a
/// concurrent committer would leave. The answer is the typed retryable 409, with no SQL text, and
/// nothing of the request is kept.
#[tokio::test]
async fn area_create_idempotency_a_key_bound_past_the_replay_check_is_a_typed_retryable_409() {
    let boot = boot().await;
    sqlx::query(
        "CREATE TRIGGER bind_raced_key AFTER INSERT ON areas WHEN NEW.name = 'Raced' BEGIN \
         INSERT INTO area_create_idempotency (creator_user_id, idempotency_key, request_fingerprint, area_id) \
         VALUES ('local-owner', 'raced', 'v1:another-request', NEW.id); END",
    )
    .execute(boot.repo.pool())
    .await
    .unwrap();
    let body = json!({"name":"Raced", "color":"#123456"});
    let (status, error) = post(&boot, Some("raced"), body.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["code"], "idempotency_key_concurrent", "{error}");
    let message = error["error"].as_str().unwrap_or_default();
    assert!(
        !message.contains("UNIQUE") && !message.contains("area_create_idempotency"),
        "no raw SQL text: {error}"
    );
    assert!(
        boot.repo
            .areas_list_user_visible()
            .await
            .unwrap()
            .is_empty()
    );
    let bindings: i64 = sqlx::query_scalar("SELECT count(*) FROM area_create_idempotency")
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        bindings, 0,
        "the whole create rolled back, the trigger's row with it"
    );

    sqlx::query("DROP TRIGGER bind_raced_key")
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let (status, area) = post(&boot, Some("raced"), body).await;
    assert_eq!(status, StatusCode::CREATED, "{area}");
}

/// Real persistent managed Track, with private bytes that the delete saga would
/// recycle. No agent or terminal launch is needed to pin the filesystem fence.
async fn managed_bytes(boot: &Boot, area_id: &str) -> PathBuf {
    use calm_server::db::sqlite::{TrackWorkspacePlan, track_create_tx};
    let root = boot._tmp.path().join("workspaces");
    let mut tx = boot.repo.pool().begin().await.unwrap();
    let track = track_create_tx(
        &mut tx,
        calm_server::model::NewTrack {
            area_id: area_id.into(),
            title: "Files".into(),
            sort: None,
            template_input: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        },
        None,
        &TrackWorkspacePlan::ManagedUnder(root.clone()),
        None,
        &calm_server::track_area_cache::TrackAreaCache::new(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let path = PathBuf::from(&track.workspace.path);
    workspace::materialize::materialize_managed_workspace(&root, &path, track.id.as_str()).unwrap();
    let sentinel = path.join("private.txt");
    std::fs::write(&sentinel, "keep private bytes").unwrap();
    sentinel
}

async fn registered_session(boot: &Boot, user_id: &str) -> String {
    let mut tx = boot.repo.pool().begin().await.unwrap();
    calm_server::db::sqlite::area_membership::application_user_register(&mut tx, user_id, user_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let token = boot.auth.sessions.create(
        calm_server::auth::SessionIdentity {
            user_id: user_id.into(),
            display_name: user_id.into(),
            role: "member".into(),
        },
        calm_server::auth::SessionAuthority::PasswordLogin,
    );
    format!("{}={token}", calm_server::auth::SESSION_COOKIE)
}

async fn request_as(
    boot: &Boot,
    cookie: &str,
    method: &str,
    uri: &str,
    body: Value,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("cookie", cookie);
    if let Some(key) = key {
        request = request.header("Idempotency-Key", key);
    }
    let response = boot
        .app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn area_ownership_authenticated_creators_have_independent_keys_and_no_owner_spoofing() {
    let boot = boot().await;
    let alice = registered_session(&boot, "alice").await;
    let body = json!({"name":"Shared key", "color":"#000", "owner_user_id":"local-owner"});
    let (owner_status, owner) = post(&boot, Some("same-key"), body.clone()).await;
    let (alice_status, area) = request_as(
        &boot,
        &alice,
        "POST",
        "/api/areas",
        body.clone(),
        Some("same-key"),
    )
    .await;
    assert_eq!(owner_status, StatusCode::CREATED);
    assert_eq!(alice_status, StatusCode::CREATED, "{area}");
    assert_ne!(owner["id"], area["id"]);
    let id = area["id"].as_str().unwrap();
    let creator: String =
        sqlx::query_scalar("SELECT user_id FROM area_members WHERE area_id=?1 AND role='owner'")
            .bind(id)
            .fetch_one(boot.repo.pool())
            .await
            .unwrap();
    assert_eq!(creator, "alice");
    let (_, replay) = request_as(
        &boot,
        &alice,
        "POST",
        "/api/areas",
        body.clone(),
        Some("same-key"),
    )
    .await;
    assert_eq!(replay["id"], area["id"]);
    sqlx::query("DELETE FROM area_members WHERE area_id=?1")
        .bind(id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let (status, _) = request_as(&boot, &alice, "POST", "/api/areas", body, Some("same-key")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM area_members WHERE area_id=?1")
            .bind(id)
            .fetch_one(boot.repo.pool())
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn area_ownership_routes_use_membership_and_distinguish_deployment_authority() {
    let boot = boot().await;
    let alice = registered_session(&boot, "alice").await;
    let outsider = registered_session(&boot, "outsider").await;
    let (_, area) = post(
        &boot,
        Some("owned"),
        json!({"name":"Private", "color":"#000"}),
    )
    .await;
    let id = area["id"].as_str().unwrap();
    let sentinel = managed_bytes(&boot, id).await;
    sqlx::query("INSERT INTO area_members VALUES(?1,'alice','member',0)")
        .bind(id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let uri = format!("/api/areas/{id}");
    let (status, updated) = request_as(
        &boot,
        &alice,
        "PATCH",
        &uri,
        json!({"name":"Collaborative"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    let (status, list) = request_as(&boot, &alice, "GET", "/api/areas", Value::Null, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (status, list) = request_as(&boot, &outsider, "GET", "/api/areas", Value::Null, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list, json!([]));
    // This invalid folder would invoke filesystem/git validation for an authorized
    // collaborator; denial must happen before those effects and their 400 result.
    let (status, _) = request_as(
        &boot,
        &outsider,
        "PATCH",
        &uri,
        json!({"default_cwd":"/definitely-missing"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = request_as(&boot, &alice, "DELETE", &uri, Value::Null, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(boot.repo.area_get(id).await.unwrap().is_some());
    assert_eq!(
        std::fs::read_to_string(&sentinel).unwrap(),
        "keep private bytes"
    );
    for target in ["/api/areas/system", "/api/areas?include_system=true"] {
        let method = if target.contains('?') { "GET" } else { "POST" };
        let (status, _) = request_as(&boot, &alice, method, target, Value::Null, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let unknown = boot.auth.sessions.create(
        calm_server::auth::SessionIdentity {
            user_id: "unregistered".into(),
            display_name: "Untrusted".into(),
            role: "owner".into(),
        },
        calm_server::auth::SessionAuthority::PasswordLogin,
    );
    let (status, _) = request_as(
        &boot,
        &format!("{}={unknown}", calm_server::auth::SESSION_COOKIE),
        "POST",
        "/api/areas",
        json!({"name":"Denied","color":"#000"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn area_ownership_delete_rechecks_owner_at_commit_and_preserves_permanent_key() {
    let boot = boot().await;
    let (_, area) = post(
        &boot,
        Some("delete-check"),
        json!({"name":"Delete","color":"#000"}),
    )
    .await;
    let id = area["id"].as_str().unwrap().to_string();
    let sentinel = managed_bytes(&boot, &id).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    calm_server::routes::areas::install_area_delete_commit_hook_for_test(
        &id,
        calm_server::routes::areas::AreaDeleteCommitHook {
            entered: entered.clone(),
            release: release.clone(),
            fail_after_release: false,
            panic_after_release: false,
        },
    );
    let app = boot.app.clone();
    let uri = format!("/api/areas/{id}");
    let owner_cookie = boot.owner_cookie.clone();
    let deletion = tokio::spawn(async move {
        app.oneshot(
            Request::builder()
                .method("DELETE")
                .header("cookie", owner_cookie)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    assert!(
        !sentinel.exists(),
        "premise: the saga recycled the workspace before the commit hook"
    );
    sqlx::query("UPDATE area_members SET role='member' WHERE area_id=?1")
        .bind(&id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    release.notify_one();
    assert_eq!(deletion.await.unwrap().status(), StatusCode::FORBIDDEN);
    assert!(boot.repo.area_get(&id).await.unwrap().is_some());
    assert_eq!(
        std::fs::read_to_string(&sentinel).unwrap(),
        "keep private bytes"
    );
    sqlx::query("UPDATE area_members SET role='owner' WHERE area_id=?1")
        .bind(&id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let response = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .header("cookie", &boot.owner_cookie)
                .uri(format!("/api/areas/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(!sentinel.exists());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM area_members WHERE area_id=?1")
            .bind(&id)
            .fetch_one(boot.repo.pool())
            .await
            .unwrap(),
        0
    );
    let (status, error) = post(
        &boot,
        Some("delete-check"),
        json!({"name":"Delete","color":"#000"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["code"], "idempotency_key_exhausted");
}
