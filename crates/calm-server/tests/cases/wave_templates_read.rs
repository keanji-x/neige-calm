//! #1209 — `GET /api/wave-templates`, the New wave picker's read side.
//!
//! The endpoint is an aggregate: `id`/`title` from the Rust template
//! constants, `input_schema` from the *bound plugin's* manifest. The two
//! tests below pin exactly that join — the same registry, once with the
//! trusted plugin running and once without it — because a read that copied
//! the schema into its own constant would pass the bound case and still be
//! wrong.

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
use calm_server::wave_cove_cache::WaveCoveCache;
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

/// The manifest fact the endpoint must surface verbatim. Deliberately NOT the
/// shipped git-forge manifest: a stub value that no other file spells proves
/// the response came from the registry rather than from a constant.
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

/// `running`: whether the trusted plugin that declares `issue-development` is
/// spawned. It is registered either way, so the only difference between the
/// two cases is the thing `resolve_trusted_workflow` actually gates on.
async fn boot(running: bool) -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();

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
            "manifest_version": 1,
            "id": plugin_id,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Trusted template owner",
            "entrypoint": { "command": "bin/stub" },
            "input_schema": stub_input_schema(),
            "workflows": [ { "id": ISSUE_DEVELOPMENT } ],
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
        calm_server::state::WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone()),
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
        Some(wave_cove_cache),
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
                .uri("/api/wave-templates")
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
        vec![ISSUE_DEVELOPMENT, SMALL_CHANGE, INVESTIGATION],
        "the read must expose exactly the kernel's template keys, in order"
    );
    // Titles come from `WORKFLOW_TEMPLATES`, not from this test's wishes.
    assert_eq!(row(&body, ISSUE_DEVELOPMENT)["title"], "Issue development");
    assert_eq!(row(&body, SMALL_CHANGE)["title"], "Small change");
    assert_eq!(row(&body, INVESTIGATION)["title"], "Investigation");
    // No `description` field anywhere — #1209: the kernel has no such fact and
    // this endpoint does not invent one.
    for entry in body.as_array().expect("array body") {
        assert!(
            entry.get("description").is_none(),
            "wave-templates must not invent a description: {entry}"
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
    // The two unbound templates are the same request, same registry: the only
    // reason they differ is the binding.
    for key in [SMALL_CHANGE, INVESTIGATION] {
        assert!(
            row(&body, key).get("input_schema").is_none(),
            "unbound template `{key}` must not advertise an input schema: {body}"
        );
    }
    // Same binding gate as create: stop the plugin and the schema goes away,
    // so the picker can never offer input the create path would then reject.
    boot.plugin_host
        .stop(&boot.plugin_id)
        .await
        .expect("stop plugin");
    let (status, body) = list_templates(boot.app).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert!(
        row(&body, ISSUE_DEVELOPMENT).get("input_schema").is_none(),
        "a stopped plugin must drop the schema, matching resolve_trusted_workflow: {body}"
    );
}

/// #1209 — the picker's tooltip lists what a template will pre-set, and the
/// only honest source for that is the template's own plan. These assertions
/// are on the *content*, not the count: a read that returned three empty
/// objects, or the wrong template's tasks, would pass a length check.
#[tokio::test]
async fn every_template_lists_the_tasks_its_report_pre_sets() {
    let boot = boot(false).await;
    let (status, body) = list_templates(boot.app).await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    let keys = |id: &str| -> Vec<String> {
        row(&body, id)["tasks"]
            .as_array()
            .unwrap_or_else(|| panic!("`{id}` must carry a tasks array: {body}"))
            .iter()
            .map(|task| task["key"].as_str().expect("task key string").to_string())
            .collect()
    };
    assert_eq!(
        keys(ISSUE_DEVELOPMENT),
        vec![
            "inspect-issue",
            "review-design-a",
            "review-design-b",
            "implement-change",
            "open-pr",
            "review-pr-a",
            "review-pr-b",
            "merge",
        ],
        "issue-development must advertise its eight pre-set tasks, in plan order"
    );
    assert_eq!(keys(SMALL_CHANGE), vec!["inspect", "implement", "verify"]);
    assert_eq!(keys(INVESTIGATION), vec!["gather-facts", "write-findings"]);

    // Every task carries a non-empty goal: the key alone is a slug, and the
    // tooltip's whole value is saying what the step is for.
    for entry in body.as_array().expect("array body") {
        for task in entry["tasks"].as_array().expect("tasks array") {
            let goal = task["goal"].as_str().expect("goal string");
            assert!(!goal.trim().is_empty(), "empty goal in {entry}");
        }
    }
    // Verbatim from `workflow_templates.rs`, not a paraphrase minted here.
    assert_eq!(
        row(&body, INVESTIGATION)["tasks"][1]["goal"],
        "Write findings, remaining unknowns, and recommended next steps into this wave report. Do not open a PR or merge."
    );

    // Listing tasks must stay a read. The template *waves* are created by the
    // create path, in a cove; if listing ever reached for a stored report
    // instead of the constants, that seed would show up right here.
    assert!(
        boot.repo.coves_list().await.expect("coves list").is_empty(),
        "listing wave templates must not write anything"
    );
}

#[tokio::test]
async fn unbound_templates_carry_no_input_schema() {
    let boot = boot(false).await;
    let (status, body) = list_templates(boot.app).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    for key in [ISSUE_DEVELOPMENT, SMALL_CHANGE, INVESTIGATION] {
        assert!(
            row(&body, key).get("input_schema").is_none(),
            "with no plugin running, `{key}` must advertise no schema: {body}"
        );
    }
}
