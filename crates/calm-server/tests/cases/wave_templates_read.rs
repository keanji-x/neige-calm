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
use calm_server::wave_area_cache::WaveAreaCache;
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
/// two cases is the thing `resolve_template_binding` actually gates on.
async fn boot(running: bool) -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let card_role_cache = CardRoleCache::new();
    let wave_area_cache = WaveAreaCache::new();
    repo.seed_wave_area_cache(&wave_area_cache).await.unwrap();

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
        calm_server::state::WriteContext::new(card_role_cache.clone(), wave_area_cache.clone()),
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
        Some(wave_area_cache),
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
    // Titles come from `TEMPLATES`, not from this test's wishes.
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
        "a stopped plugin must drop the schema, matching resolve_template_binding: {body}"
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
    // Verbatim from `templates.rs`, not a paraphrase minted here.
    assert_eq!(
        row(&body, INVESTIGATION)["tasks"][1]["goal"],
        "Write findings, remaining unknowns, and recommended next steps into this wave report. Do not open a PR or merge."
    );

    // Listing tasks must stay a read. The template *waves* are created by the
    // create path, in an area; if listing ever reached for a stored report
    // instead of the constants, that seed would show up right here.
    assert!(
        boot.repo.areas_list().await.expect("areas list").is_empty(),
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

/// #1300 S1 — the template **write** endpoint is gone, and this is the
/// assertion that says so.
///
/// `PUT /api/wave-templates/{id}` and the Settings › Templates editor existed
/// between #1230 and #1300. They were built on the seeded template wave, which
/// #1300 removes because it is the last production path on which the kernel
/// writes a report as `EditAuthor::User`.
///
/// ## Why a deleted route needs a test at all
///
/// Deleting a handler and deleting nothing else both look like "the editor is
/// gone" in a diff. The difference is observable only from outside: a route
/// that is still registered but reaches dead code, a router that falls through
/// to some catch-all, or a re-added handler in a later change all pass a
/// review that only reads the deletion. So this asserts the two things a
/// caller can see, and both halves matter:
///
///  * the method is **not routed** — `405` (the path exists for `GET`) or
///    `404`, never a 2xx and never a 5xx from a handler that ran;
///  * **nothing was written**. A rejection that still committed something on
///    its way to the rejection would satisfy the status check alone.
///
/// The body is a well-formed `WaveTemplateUpdate` as the deleted endpoint
/// accepted it, so this fails if the route comes back *and works*, not merely
/// if the wire shape drifts.
#[tokio::test]
async fn put_is_not_routed_and_writes_nothing() {
    let boot = boot(false).await;

    // Every roster key, not just one. A residual route could easily be
    // reintroduced for a subset — a `match id` that handles one template and
    // falls through for the rest is a perfectly ordinary shape — and a
    // single-key check would call that gone.
    for id in [ISSUE_DEVELOPMENT, SMALL_CHANGE, INVESTIGATION] {
        let before = db_digest(&boot.repo).await;

        let resp = boot
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/wave-templates/{id}"))
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

        // 404 **with an empty body**, which is the discriminator that makes
        // this an assertion about routing rather than about a status code.
        //
        // The path `/api/wave-templates/{id}` is not registered for any method,
        // so axum's own fallback answers — and its 404 carries no body. A
        // *handler* that ran and chose to refuse cannot produce that: every
        // refusal in this kernel goes through `CalmError`, which renders a JSON
        // `ErrorBody`. Accepting any 404 would have let a restored handler that
        // writes on its way to answering `NotFound` pass — the exact
        // construction a reviewer proposed against the first version of this
        // test, and it would have been green.
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "PUT /api/wave-templates/{id} must not be routed; body={:?}",
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
///
/// Deliberately not "count the waves" — a write the removal was supposed to
/// prevent could land in `cards`, `overlays` or `events` and leave the wave
/// count alone. Comparing the whole database is the only shape that does not
/// require guessing which table a resurrected handler would touch.
async fn db_digest(repo: &Arc<dyn Repo>) -> Vec<(String, String)> {
    let pool = repo.sqlite_pool().expect("sqlite pool");
    // `sqlite_sequence` is deliberately NOT excluded. It is an ordinary
    // writable table holding the AUTOINCREMENT high-water mark, so an insert
    // that is rolled back — or inserted and deleted — still advances it. That
    // is precisely the "wrote something on its way to refusing" shape this
    // digest exists to catch, and a blanket `name NOT LIKE 'sqlite_%'` would
    // have hidden it. The other `sqlite_*` objects are internal indices with no
    // rows of their own; `type = 'table'` already excludes them.
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
