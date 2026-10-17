//! Settings persistence + REST end-to-end: the `Repo` KV surface and `GET`/`PUT /api/settings`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn fresh_state() -> (AppState, Arc<SqlxRepo>) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );
    (state, repo)
}

#[tokio::test]
async fn repo_round_trips_settings_kv() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    assert!(repo.settings_get_all().await.unwrap().is_empty());

    repo.settings_upsert("http_proxy", "http://127.0.0.1:10809")
        .await
        .unwrap();
    repo.settings_upsert("https_proxy", "http://127.0.0.1:10809")
        .await
        .unwrap();
    let mut rows = repo.settings_get_all().await.unwrap();
    rows.sort();
    assert_eq!(
        rows,
        vec![
            (
                "http_proxy".to_string(),
                "http://127.0.0.1:10809".to_string()
            ),
            (
                "https_proxy".to_string(),
                "http://127.0.0.1:10809".to_string()
            ),
        ]
    );

    repo.settings_upsert("http_proxy", "http://proxy.example:3128")
        .await
        .unwrap();
    let rows = repo.settings_get_all().await.unwrap();
    let http = rows.iter().find(|(k, _)| k == "http_proxy").unwrap();
    assert_eq!(http.1, "http://proxy.example:3128");

    repo.settings_delete("http_proxy").await.unwrap();
    repo.settings_delete("http_proxy").await.unwrap();
    let rows = repo.settings_get_all().await.unwrap();
    assert!(rows.iter().all(|(k, _)| k != "http_proxy"));
}

#[tokio::test]
async fn get_settings_returns_empty_bag_initially() {
    let (state, _repo) = fresh_state().await;
    let app = routes::application_router(state, super::auth::dev_auth_state());

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(v["settings"].is_object());
    assert_eq!(v["settings"].as_object().unwrap().len(), 0);
}

#[tokio::test]
async fn put_then_get_round_trips_proxy() {
    let (state, _repo) = fresh_state().await;
    let app = routes::application_router(state, super::auth::dev_auth_state());

    let body = serde_json::json!({
        "settings": {
            "http_proxy": "http://10.0.0.5:3128",
            "https_proxy": "http://10.0.0.5:3128",
        }
    })
    .to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/settings")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["settings"]["http_proxy"], "http://10.0.0.5:3128");
    assert_eq!(v["settings"]["https_proxy"], "http://10.0.0.5:3128");

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["settings"]["http_proxy"], "http://10.0.0.5:3128");
}

#[tokio::test]
async fn put_with_empty_or_null_clears_key() {
    let (state, repo) = fresh_state().await;
    repo.settings_upsert("http_proxy", "http://10.0.0.5:3128")
        .await
        .unwrap();
    repo.settings_upsert("https_proxy", "http://10.0.0.5:3128")
        .await
        .unwrap();

    let app = routes::application_router(state, super::auth::dev_auth_state());

    let body = serde_json::json!({
        "settings": {
            "http_proxy": "",
            "https_proxy": serde_json::Value::Null,
        }
    })
    .to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/settings")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let obj = v["settings"].as_object().unwrap();
    assert!(!obj.contains_key("http_proxy"));
    assert!(!obj.contains_key("https_proxy"));
}

#[tokio::test]
async fn settings_loader_picks_up_proxy_for_codex_spawn() {
    use calm_server::routes::settings::load_settings;
    let (state, repo) = fresh_state().await;
    repo.settings_upsert("http_proxy", "http://corp.proxy:8080")
        .await
        .unwrap();

    let s = load_settings(state.repo.as_ref()).await.unwrap();
    assert_eq!(s.http_proxy.as_deref(), Some("http://corp.proxy:8080"));
    assert!(s.https_proxy.is_none());
}

#[tokio::test]
async fn settings_proxy_update_marks_needs_respawn() {
    let (state, _repo) = fresh_state().await;
    let shared = state.shared_codex_appserver.clone();
    let app = routes::application_router(state, super::auth::dev_auth_state());

    let body = serde_json::json!({
        "settings": {
            "http_proxy": "http://proxy.changed:3128"
        }
    })
    .to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/settings")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert!(shared.needs_respawn_on_next_thread_start_for_test());
}

#[tokio::test]
async fn settings_proxy_update_with_unchanged_value_does_not_mark() {
    let (state, repo) = fresh_state().await;
    repo.settings_upsert("http_proxy", "http://proxy.same:3128")
        .await
        .unwrap();
    let shared = state.shared_codex_appserver.clone();
    let app = routes::application_router(state, super::auth::dev_auth_state());

    let body = serde_json::json!({
        "settings": {
            "http_proxy": "http://proxy.same:3128"
        }
    })
    .to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/settings")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert!(!shared.needs_respawn_on_next_thread_start_for_test());
}

#[tokio::test]
async fn planner_turn_budget_invalid_patch_writes_nothing() {
    let (state, repo) = fresh_state().await;
    let shared = state.shared_codex_appserver.clone();
    let app = routes::application_router(state, super::auth::dev_auth_state());
    for value in [
        "0",
        "241",
        " 30",
        "+30",
        "1.0",
        "1e1",
        "３０",
        "999999999999999999999999",
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/settings")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"settings": {
                            "aaa_unknown": "must not write", "http_proxy": "changed",
                            "planner_max_turn_duration_minutes": value
                        }})
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 400, "{value}");
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&bytes).contains("planner_max_turn_duration_minutes"));
        assert!(repo.settings_get_all().await.unwrap().is_empty());
        assert!(!shared.needs_respawn_on_next_thread_start_for_test());
    }
}

#[tokio::test]
async fn planner_turn_budget_canonical_reset_and_corrupt_get() {
    let (state, repo) = fresh_state().await;
    let app = routes::application_router(state, super::auth::dev_auth_state());
    for (value, expected) in [
        (serde_json::json!("0001"), Some("1")),
        (serde_json::json!("240"), Some("240")),
        (serde_json::Value::Null, None),
        (serde_json::json!(""), None),
    ] {
        repo.settings_upsert("planner_max_turn_duration_minutes", "broken")
            .await
            .unwrap();
        let get = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/settings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get.status(), 200);
        let bytes = get.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&bytes).contains("broken"));
        let resp = app.clone().oneshot(Request::builder().method("PUT").uri("/api/settings")
            .header("content-type", "application/json").body(Body::from(serde_json::json!({"settings": {
                "planner_max_turn_duration_minutes": value, "unknown": "unchanged semantics"
            }}).to_string())).unwrap()).await.unwrap();
        assert_eq!(resp.status(), 200);
        let rows = repo.settings_get_all().await.unwrap();
        assert_eq!(
            rows.iter()
                .find(|(k, _)| k == "planner_max_turn_duration_minutes")
                .map(|(_, v)| v.as_str()),
            expected
        );
        assert!(
            rows.iter()
                .any(|(k, v)| k == "unknown" && v == "unchanged semantics")
        );
    }
}

async fn area_owner_session() -> (axum::Router, AppState, Arc<SqlxRepo>, String) {
    let (state, repo) = fresh_state().await;
    let auth = super::auth::live_auth_state("owner", "fixture-password");
    let user = "settings-alice";
    let mut tx = repo.pool().begin().await.unwrap();
    calm_server::db::sqlite::area_membership::application_user_register(&mut tx, user, "Alice")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let token = auth.sessions.create(
        calm_server::auth::SessionIdentity {
            user_id: user.into(),
            // These credential-bound labels still do not confer deployment ownership.
            display_name: "Owner".into(),
            role: calm_server::auth::OWNER_ROLE.into(),
        },
        calm_server::auth::SessionAuthority::PasswordLogin,
    );
    let cookie = format!("{}={token}", calm_server::auth::SESSION_COOKIE);
    let app = routes::application_router(state.clone(), auth);
    let response = app
        .clone()
        .oneshot(
            Request::post("/api/areas")
                .header("content-type", "application/json")
                .header("cookie", &cookie)
                .body(Body::from(r##"{"name":"Alice's Area","color":"#abc"}"##))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let area: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let membership = repo
        .area_require_access(
            area["id"].as_str().unwrap(),
            user,
            calm_types::organization::AreaAccess::Delete,
        )
        .await
        .unwrap();
    assert_eq!(membership.id.as_str(), area["id"].as_str().unwrap());
    (app, state, repo, cookie)
}

fn nonowner_request(method: &str, cookie: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri("/api/settings")
        .header("content-type", "application/json")
        .header("cookie", cookie)
        .header("x-calm-actor", "user")
        .header("x-calm-user-id", calm_server::auth::OWNER_USER_ID)
        .header("x-calm-role", calm_server::auth::OWNER_ROLE)
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn settings_nonowner_cannot_read_installation_values() {
    let (app, _state, repo, cookie) = area_owner_session().await;
    repo.settings_upsert(
        "http_proxy",
        "http://fixture-user:fixture-secret@proxy.invalid:3128",
    )
    .await
    .unwrap();
    repo.settings_upsert("private_setting", "installation-only-value")
        .await
        .unwrap();
    let response = app
        .oneshot(nonowner_request("GET", &cookie, serde_json::json!({})))
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(
        status == 403
            && !text.contains("fixture-secret")
            && !text.contains("installation-only-value"),
        "nonowner status={status}, leaked installation values: {text}"
    );
}

#[tokio::test]
async fn settings_nonowner_cannot_write_or_reconfigure_installation() {
    let (app, state, repo, cookie) = area_owner_session().await;
    repo.settings_upsert("http_proxy", "http://original.invalid:3128")
        .await
        .unwrap();
    let before = repo.settings_get_all().await.unwrap();
    for duration in ["30", "not-a-duration"] {
        let response = app
            .clone()
            .oneshot(nonowner_request(
                "PUT",
                &cookie,
                serde_json::json!({"settings": {
                    "http_proxy":"http://changed.invalid:3128", "aaa_unknown":"must not write",
                    "planner_max_turn_duration_minutes":duration
                }}),
            ))
            .await
            .unwrap();
        let status = response.status();
        let after = repo.settings_get_all().await.unwrap();
        let respawn = state
            .shared_codex_appserver
            .needs_respawn_on_next_thread_start_for_test();
        assert!(
            status == 403 && before == after && !respawn,
            "nonowner duration={duration}, status={status}, rows_changed={}, respawn={respawn}",
            before != after
        );
    }
}

#[tokio::test]
async fn settings_password_owner_reads_and_updates_installation_values() {
    let (state, repo) = fresh_state().await;
    let shared = state.shared_codex_appserver.clone();
    let app = routes::application_router(
        state,
        super::auth::live_auth_state("owner", "fixture-password"),
    );
    let login = app
        .clone()
        .oneshot(
            Request::post("/api/auth/login")
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    4444,
                ))))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"username":"owner","password":"fixture-password"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), 200);
    let cookie = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let response = app.clone().oneshot(Request::put("/api/settings")
        .header("content-type", "application/json").header("cookie", &cookie)
        .body(Body::from(r#"{"settings":{"HTTPS_PROXY":"http://owner.invalid:3128","private_setting":"owner-value"}}"#)).unwrap()).await.unwrap();
    assert_eq!(response.status(), 200);
    assert!(shared.needs_respawn_on_next_thread_start_for_test());
    assert_eq!(
        routes::settings::load_settings(repo.as_ref())
            .await
            .unwrap()
            .https_proxy
            .as_deref(),
        Some("http://owner.invalid:3128")
    );
    let response = app
        .oneshot(
            Request::get("/api/settings")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let bag: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(bag["settings"]["private_setting"], "owner-value");
}

#[tokio::test]
async fn settings_requires_a_live_session_for_reads_and_writes() {
    let (state, repo) = fresh_state().await;
    let shared = state.shared_codex_appserver.clone();
    let app = routes::application_router(
        state,
        super::auth::live_auth_state("owner", "fixture-password"),
    );
    repo.settings_upsert("private_setting", "session-protected-value")
        .await
        .unwrap();
    let before = repo.settings_get_all().await.unwrap();
    let unknown = format!("{}=unknown-session", calm_server::auth::SESSION_COOKIE);
    for cookie in [None, Some(unknown.as_str())] {
        for method in ["GET", "PUT"] {
            let mut request = Request::builder()
                .method(method)
                .uri("/api/settings")
                .header("content-type", "application/json");
            if let Some(cookie) = cookie {
                request = request.header("cookie", cookie);
            }
            let response = app
                .clone()
                .oneshot(
                    request
                        .body(Body::from(
                            r#"{"settings":{"http_proxy":"http://must-not-write.invalid:3128"}}"#,
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), 401);
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            assert!(!String::from_utf8_lossy(&bytes).contains("session-protected-value"));
            assert_eq!(repo.settings_get_all().await.unwrap(), before);
            assert!(!shared.needs_respawn_on_next_thread_start_for_test());
        }
    }
}
