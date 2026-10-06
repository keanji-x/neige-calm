//! `POST /api/plugins/{id}/enable|disable` set the state they name (#2132 item 2, #2175 item 5).
//! A refusal leaves the row as the request found it; a 503 wait keeps the enable it answers about,
//! and the list carries the reason as the plugin's `last_error`.

#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::{Instant, sleep};
use tower::ServiceExt;

const ECHO_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-echo");

struct Fx {
    state: AppState,
    repo: Arc<dyn Repo>,
    _tmp: TempDir,
}

/// An `app` plugin over the echo stub; `overrides` replaces top-level manifest keys.
fn write_app(plugins_dir: &Path, id: &str, overrides: &Value) {
    let dir = plugins_dir.join(id);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::os::unix::fs::symlink(Path::new(ECHO_BIN), dir.join("bin").join("stub")).unwrap();
    let mut manifest = json!({
        "manifest_version": 1,
        "id": id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": id,
        "entrypoint": { "command": "bin/stub" },
    });
    for (key, value) in overrides.as_object().unwrap() {
        manifest[key] = value.clone();
    }
    std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
}

/// A host whose registry and rows are seeded as a boot would find them: `(id, enabled, overrides)`.
async fn boot(plugins: &[(&str, bool, Value)]) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let (plugins_dir, data_dir) = (tmp.path().join("plugins"), tmp.path().join("data"));
    std::fs::create_dir_all(&plugins_dir).unwrap();
    std::fs::create_dir_all(&data_dir).unwrap();
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    for (id, enabled, overrides) in plugins {
        write_app(&plugins_dir, id, overrides);
        repo.plugin_install(calm_server::model::NewPlugin {
            id: (*id).into(),
            version: "0.1.0".into(),
            install_path: plugins_dir.join(id).display().to_string(),
            manifest: json!({}),
            enabled: *enabled,
            user_config: json!({}),
        })
        .await
        .unwrap();
    }
    let (registry, report) = PluginRegistry::load_from_dir(&plugins_dir).unwrap();
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    let events = EventBus::new();
    let host = Arc::new(PluginHost::new_full(
        Arc::new(registry),
        repo.clone(),
        plugins_dir,
        data_dir,
        Vec::new(),
        events.clone(),
        calm_server::state::WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::track_area_cache::TrackAreaCache::new(),
        ),
    ));
    let state = AppState::from_parts(
        repo.clone(),
        events,
        Arc::new(DaemonClient::new_stub()),
        host,
        Arc::new(calm_server::state::CodexClient::new_stub()),
        None,
        None,
    );
    Fx {
        state,
        repo,
        _tmp: tmp,
    }
}

async fn call(fx: &Fx, method: &str, path: &str) -> (StatusCode, Value) {
    let resp = axum::Router::new()
        .merge(routes::plugins::router())
        .with_state(fx.state.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(if method == "GET" { "" } else { "{}" }))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn row_enabled(fx: &Fx, id: &str) -> bool {
    fx.repo.plugin_get_by_id(id).await.unwrap().unwrap().enabled
}

async fn wait_running(fx: &Fx, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (_, detail) = call(fx, "GET", &format!("/api/plugins/{id}")).await;
        if detail["state"] == "running" {
            return;
        }
        assert!(Instant::now() < deadline, "{id} never ran: {detail}");
        sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn enabling_a_running_plugin_answers_200_with_its_state() {
    let fx = boot(&[("echoa", false, json!({}))]).await;
    let (status, _) = call(&fx, "POST", "/api/plugins/echoa/enable").await;
    assert_eq!(status, StatusCode::OK);
    wait_running(&fx, "echoa").await;

    let (status, body) = call(&fx, "POST", "/api/plugins/echoa/enable").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "enable names a state, and the plugin is in it: {body}"
    );
    assert_eq!(body["enabled"], true);
    assert_eq!(body["state"], "running");

    call(&fx, "POST", "/api/plugins/echoa/disable").await;
}

#[tokio::test]
async fn disabling_a_stopped_plugin_answers_200_with_its_state() {
    let fx = boot(&[("echob", false, json!({}))]).await;
    let (status, body) = call(&fx, "POST", "/api/plugins/echob/disable").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], false);
    assert_eq!(body["state"], "disabled");
}

/// A 422 is a refusal, so the row it answers about is the row the request found.
#[tokio::test]
async fn a_kernel_too_old_refusal_leaves_the_row_disabled() {
    let fx = boot(&[("toonew", false, json!({ "min_kernel_version": "99.0.0" }))]).await;
    let (status, body) = call(&fx, "POST", "/api/plugins/toonew/enable").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "plugin_kernel_too_old");
    assert!(
        !row_enabled(&fx, "toonew").await,
        "a refused enable must not leave `enabled = true` behind"
    );
}

/// A 409 conflict restores the bit the request found, whichever it was: a row already enabled
/// (as a boot leaves one whose spawn failed) stays enabled, so autospawn keeps trying it.
#[tokio::test]
async fn a_minted_name_refusal_leaves_the_row_as_it_found_it() {
    for found in [false, true] {
        let fx = boot(&[("mint-a", false, json!({})), ("mint.a", found, json!({}))]).await;
        let (status, _) = call(&fx, "POST", "/api/plugins/mint-a/enable").await;
        assert_eq!(status, StatusCode::OK);
        wait_running(&fx, "mint-a").await;

        let (status, body) = call(&fx, "POST", "/api/plugins/mint.a/enable").await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "plugin_conflict");
        assert_eq!(
            row_enabled(&fx, "mint.a").await,
            found,
            "the refusal must leave `enabled` as the request found it ({found})"
        );
        call(&fx, "POST", "/api/plugins/mint-a/disable").await;
    }
}

/// A 503 is the enable having landed: the row stays enabled, and the reason is the plugin's
/// `last_error` in the list a client re-reads.
#[tokio::test]
async fn a_wait_for_configuration_keeps_the_enable_and_lists_its_reason() {
    let fx = boot(&[(
        "needskey",
        false,
        json!({
            "manifest_version": 3,
            "config_schema": {
                "type": "object",
                "properties": { "token": { "type": "string" } },
                "required": ["token"],
                "additionalProperties": false
            }
        }),
    )])
    .await;
    let (status, body) = call(&fx, "POST", "/api/plugins/needskey/enable").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "service_unavailable");
    assert!(
        row_enabled(&fx, "needskey").await,
        "a wait keeps `enabled = true`"
    );

    let (_, list) = call(&fx, "GET", "/api/plugins").await;
    let item = &list[0];
    assert_eq!(item["id"], "needskey");
    assert_eq!(item["enabled"], true);
    assert_eq!(item["state"], "unavailable");
    let reason = item["last_error"]
        .as_str()
        .expect("the list names the wait");
    assert!(
        body["error"].as_str().unwrap().ends_with(reason),
        "the list carries the reason the enable answered: {reason:?} vs {body}"
    );
}
