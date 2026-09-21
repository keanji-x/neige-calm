//! `GET /api/track-templates`, the New track picker's read side.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::NewPlugin;
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::{Instant, sleep};
use tower::ServiceExt;

use crate::common;

const ECHO_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-echo");
const ISSUE_DEVELOPMENT: &str = "issue-development";
const SMALL_CHANGE: &str = "small-change";
const INVESTIGATION: &str = "investigation";
const INVESTMENT_RESEARCH: &str = "investment-research";

/// Mirrors `forge_trust::trusted_forge_plugin`'s default so the stub is
/// trusted without mutating process env.
fn trusted_plugin_id() -> String {
    std::env::var("NEIGE_TRUSTED_FORGE_PLUGINS")
        .ok()
        .and_then(|configured| {
            configured
                .split(',')
                .map(str::trim)
                .find(|id| !id.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "dev.neige.git-forge".to_string())
}

/// A stub value no other file spells, so the response provably came from the registry rather than a constant.
fn stub_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "issue_url": { "type": "string" } },
        "required": ["issue_url"],
        "additionalProperties": false
    })
}

struct Boot {
    app: axum::Router,
    plugin_host: Arc<PluginHost>,
    plugin_id: String,
    repo: Arc<dyn Repo>,
    _tmp: TempDir,
}

/// `running`: whether the trusted plugin is spawned; it is registered either way.
async fn boot(running: bool) -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();

    let plugin_id = trusted_plugin_id();
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let install_dir = plugins_dir.join(&plugin_id);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("create plugin bin dir");
    std::fs::create_dir_all(&plugins_data_dir).expect("create plugin data dir");
    std::os::unix::fs::symlink(Path::new(ECHO_BIN), bin_dir.join("stub"))
        .expect("symlink stub plugin");

    let manifest: Manifest = Manifest::parse(
        &json!({
            "manifest_version": 2,
            "id": plugin_id,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Trusted template owner",
            "entrypoint": { "command": "bin/stub" },
            "input_schema": stub_input_schema(),
            "templates": [ { "id": ISSUE_DEVELOPMENT } ],
            "permissions": {}
        })
        .to_string(),
    )
    .expect("manifest parses");
    let registry = PluginRegistry::from_manifests([(manifest, Some(install_dir.clone()))]);
    repo.plugin_install(NewPlugin {
        id: plugin_id.clone(),
        version: "0.1.0".into(),
        install_path: install_dir.display().to_string(),
        manifest: json!({}),
        enabled: true,
        user_config: json!({}),
    })
    .await
    .expect("seed plugin row");

    let plugin_host = Arc::new(PluginHost::new_full(
        Arc::new(registry),
        repo.clone(),
        plugins_dir,
        plugins_data_dir,
        Vec::new(),
        EventBus::new(),
        calm_server::state::WriteContext::new(card_role_cache.clone(), track_area_cache.clone()),
    ));
    if running {
        plugin_host.spawn(&plugin_id).await.expect("spawn plugin");
        wait_for_running(&plugin_host, &plugin_id).await;
    }

    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient {
            data_dir: tmp.path().to_path_buf(),
            proc_supervisor_sock: None,
        }),
        plugin_host.clone(),
        Arc::new(common::fake_codex_client()),
        Some(card_role_cache),
        Some(track_area_cache),
    );
    let state = state.with_shared_codex_appserver(
        SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None),
    );
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);
    Boot {
        app,
        plugin_host,
        plugin_id,
        repo,
        _tmp: tmp,
    }
}

async fn wait_for_running(host: &Arc<PluginHost>, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(s) = host.status(id).await
            && matches!(s.status, PluginRuntimeStatus::Running)
        {
            return;
        }
        assert!(
            Instant::now() <= deadline,
            "plugin {id} did not reach Running within 5s"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

async fn list_templates(app: axum::Router) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/track-templates")
                .header("X-Calm-Actor", "user")
                .body(Body::empty())
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

fn row<'a>(body: &'a Value, id: &str) -> &'a Value {
    body.as_array()
        .expect("array body")
        .iter()
        .find(|entry| entry["id"] == id)
        .unwrap_or_else(|| panic!("template `{id}` missing from {body}"))
}

#[tokio::test]
async fn lists_every_template_with_its_kernel_title() {
    let boot = boot(false).await;
    let (status, body) = list_templates(boot.app).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let ids: Vec<&str> = body
        .as_array()
        .expect("array body")
        .iter()
        .map(|entry| entry["id"].as_str().expect("id string"))
        .collect();
    assert_eq!(
        ids,
        vec![
            ISSUE_DEVELOPMENT,
            SMALL_CHANGE,
            INVESTIGATION,
            INVESTMENT_RESEARCH
        ],
        "the read must expose exactly the kernel's template keys, in order"
    );
    assert_eq!(row(&body, ISSUE_DEVELOPMENT)["title"], "Issue development");
    assert_eq!(row(&body, SMALL_CHANGE)["title"], "Small change");
    assert_eq!(row(&body, INVESTIGATION)["title"], "Investigation");
    assert_eq!(
        row(&body, INVESTMENT_RESEARCH)["title"],
        "Investment research"
    );
    for entry in body.as_array().expect("array body") {
        assert!(
            entry.get("description").is_none(),
            "track-templates must not invent a description: {entry}"
        );
    }
}

#[tokio::test]
async fn bound_template_carries_the_plugin_input_schema() {
    let boot = boot(true).await;
    let (status, body) = list_templates(boot.app.clone()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(
        row(&body, ISSUE_DEVELOPMENT)["input_schema"],
        stub_input_schema(),
        "a bound template must carry its owning plugin's manifest schema verbatim"
    );
    for key in [SMALL_CHANGE, INVESTIGATION, INVESTMENT_RESEARCH] {
        assert!(
            row(&body, key).get("input_schema").is_none(),
            "unbound template `{key}` must not advertise an input schema: {body}"
        );
    }
    // Same binding gate as create: stop the plugin and the schema goes away.
    boot.plugin_host
        .stop(&boot.plugin_id)
        .await
        .expect("stop plugin");
    let (status, body) = list_templates(boot.app).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert!(
        row(&body, ISSUE_DEVELOPMENT).get("input_schema").is_none(),
        "a stopped plugin must drop the schema, matching resolve_template_binding: {body}"
    );
}

/// A template no longer promises placeholder tasks in the picker.
#[tokio::test]
async fn every_template_lists_no_preset_tasks() {
    let boot = boot(false).await;
    let (status, body) = list_templates(boot.app).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for id in [
        ISSUE_DEVELOPMENT,
        SMALL_CHANGE,
        INVESTIGATION,
        INVESTMENT_RESEARCH,
    ] {
        assert_eq!(row(&body, id)["tasks"], json!([]), "{id}");
    }
    assert!(
        boot.repo.areas_list().await.unwrap().is_empty(),
        "listing remains read-only"
    );
}

#[tokio::test]
async fn unbound_templates_carry_no_input_schema() {
    let boot = boot(false).await;
    let (status, body) = list_templates(boot.app).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    for key in [
        ISSUE_DEVELOPMENT,
        SMALL_CHANGE,
        INVESTIGATION,
        INVESTMENT_RESEARCH,
    ] {
        assert!(
            row(&body, key).get("input_schema").is_none(),
            "with no plugin running, `{key}` must advertise no schema: {body}"
        );
    }
}

#[tokio::test]
async fn put_is_not_routed_and_writes_nothing() {
    let boot = boot(false).await;

    for template in calm_server::templates::TemplateRoster::builtin()
        .entries()
        .iter()
    {
        let id = template.key();
        let before = db_digest(&boot.repo).await;

        let resp = boot
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/track-templates/{id}"))
                    .header("X-Calm-Actor", "user")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "title": "Renamed by a caller that should not exist",
                            "edits": [ { "key": "inspect", "goal": "rewritten" } ],
                            "appends": []
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let body = resp.into_body().collect().await.unwrap().to_bytes();

        // 404 with an empty body: axum's fallback carries no body, while every handler refusal renders a JSON `ErrorBody`.
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "PUT /api/track-templates/{id} must not be routed; body={:?}",
            String::from_utf8_lossy(&body)
        );
        assert!(
            body.is_empty(),
            "a 404 with a body came from a handler, not from the router; body={:?}",
            String::from_utf8_lossy(&body)
        );

        assert_eq!(
            db_digest(&boot.repo).await,
            before,
            "{id}: a PUT that is not routed must not have written anything"
        );
    }
}

/// Whole-database content digest: every table, every row, in a stable order.
async fn db_digest(repo: &Arc<dyn Repo>) -> Vec<(String, String)> {
    let pool = repo.sqlite_pool().expect("sqlite pool");
    // `sqlite_sequence` is deliberately included: a rolled-back insert still advances the AUTOINCREMENT high-water mark.
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master \
         WHERE type = 'table' AND name <> '_sqlx_migrations' \
         AND name NOT LIKE 'sqlite_stat%' \
         ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .expect("table list");
    assert!(!tables.is_empty(), "digest found no tables to compare");
    let mut digest = Vec::with_capacity(tables.len());
    for table in tables {
        let columns: Vec<String> =
            sqlx::query_scalar(&format!("SELECT name FROM pragma_table_info('{table}')"))
                .fetch_all(&pool)
                .await
                .unwrap_or_else(|error| panic!("columns of {table}: {error}"));
        let row_text = columns
            .iter()
            .map(|column| format!("quote(\"{column}\")"))
            .collect::<Vec<_>>()
            .join(" || '|' || ");
        let rows: String = sqlx::query_scalar(&format!(
            "SELECT coalesce(group_concat(row_text, char(10)), '') FROM \
             (SELECT {row_text} AS row_text FROM \"{table}\" ORDER BY 1)"
        ))
        .fetch_one(&pool)
        .await
        .unwrap_or_else(|error| panic!("digest of {table}: {error}"));
        digest.push((table, rows));
    }
    digest
}
