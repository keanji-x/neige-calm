//! Integration tests for `/api/plugins/*` (Slice D).
//!
//! Boots a minimal Axum app with the plugins router + AppState (MockRepo,
//! EventBus, stub DaemonClient, real PluginHost rooted in a tempdir), then
//! drives the REST surface via an in-process HTTP client. We re-use the
//! `plugin-host-stub-echo` binary from Slice B as the spawnable plugin
//! payload — it answers `initialize` and idles until SIGTERM, which is all
//! the supervisor + the routes layer care about.
//!
//! What we cover (eight scenarios per Slice D's binding planner):
//!
//!   1. install + list flow
//!   2. enable spawns the process
//!   3. disable stops the process
//!   4. log endpoint returns stderr
//!   5. uninstall cascades tokens / kv / overlays
//!   6. views catalog reflects the installed manifest
//!   7. install rejects manifest with disallowed `scope`
//!   8. install rejects reinstall with 409
//!
//! Plus the #1196 S0b lifecycle contract tests at the bottom of the file: the
//! four unknown-id 404s and reload's conditional respawn.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::NewOverlay;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::{Instant, sleep};
use tower::ServiceExt;

const ECHO_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-echo");

// ---------------------------------------------------------------------------
// Test fixture: build a plugin directory on disk containing a valid manifest
// and a symlink to the echo stub binary at `bin/stub`. The manifest carries
// one view so the views-catalog test has a non-empty payload to assert on.
// ---------------------------------------------------------------------------

fn write_stub_plugin(plugins_dir: &Path, id: &str) -> PathBuf {
    let plugin_dir = plugins_dir.join(id);
    let bin_dir = plugin_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::os::unix::fs::symlink(Path::new(ECHO_BIN), bin_dir.join("stub")).unwrap();
    let manifest = json!({
        "manifest_version": 1,
        "id": id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "Echo Stub",
        "description": "test fixture",
        "entrypoint": { "command": "bin/stub" },
        "views": [
            {
                "view_id": "main",
                "title": "Echo View",
                "scope": "card",
                "default_size": { "w": 4, "h": 3 }
            }
        ],
        "permissions": {
            "overlays_write": ["track", "card"],
            "cards_create": true,
            "kv_quota_bytes": 65536
        }
    });
    std::fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    plugin_dir
}

fn write_bad_scope_plugin(plugins_dir: &Path, id: &str) -> PathBuf {
    let plugin_dir = plugins_dir.join(id);
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let manifest = json!({
        "manifest_version": 1,
        "id": id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "Bad Scope",
        "entrypoint": { "command": "bin/stub" },
        "views": [
            { "view_id": "wide", "title": "Wide", "scope": "track" }
        ]
    });
    std::fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    plugin_dir
}

/// Build an `AppState` rooted in a fresh tempdir, an empty `PluginRegistry`,
/// and an in-memory `SqlxRepo`. Returns the state, a holding TempDir (drops
/// cleanup), and the resolved `plugins_dir` so tests can drop fixtures into it.
async fn boot_state() -> (AppState, TempDir, PathBuf) {
    let (state, tmp, plugins_dir, _repo) = boot_state_with_repo().await;
    (state, tmp, plugins_dir)
}

/// `boot_state`, plus the `Repo` handle — for the two #1284 S1 tests that have
/// to put the DB into a state no route can produce (a manifest blob written by
/// an older kernel; a corrupt `user_config`).
async fn boot_state_with_repo() -> (AppState, TempDir, PathBuf, Arc<dyn Repo>) {
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    std::fs::create_dir_all(&plugins_data_dir).unwrap();
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite repo"),
    );
    let events = EventBus::new();
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        repo.clone(),
        plugins_dir.clone(),
        plugins_data_dir,
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
        plugin,
        Arc::new(calm_server::state::CodexClient::new_stub()),
        None, // PR3 (#136): card_role_cache — tests don't exercise role gating
        None, // #234: track_area_cache — same rationale
    );
    (state, tmp, plugins_dir, repo)
}

fn app(state: AppState) -> axum::Router {
    axum::Router::new()
        .merge(routes::plugins::router())
        .with_state(state)
}

async fn body_to_json(resp: axum::http::Response<Body>) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn body_to_text(resp: axum::http::Response<Body>) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

async fn post_json(app: axum::Router, path: &str, body: Value) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn get_path(app: axum::Router, path: &str) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("GET")
            .uri(path)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn delete_path(app: axum::Router, path: &str) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("DELETE")
            .uri(path)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

/// Poll `GET /api/plugins/:id` until `state` matches `expected` (wire string)
/// or `deadline` is exceeded. Returns the final detail JSON so the caller can
/// assert further.
async fn wait_for_state(state: &AppState, id: &str, expected: &str, timeout: Duration) -> Value {
    let start = Instant::now();
    loop {
        let resp = get_path(app(state.clone()), &format!("/api/plugins/{id}")).await;
        let json = body_to_json(resp).await;
        if json.get("state").and_then(|v| v.as_str()) == Some(expected) {
            return json;
        }
        if start.elapsed() > timeout {
            panic!(
                "timeout waiting for state `{expected}` (got {:?}, elapsed {:?})",
                json.get("state"),
                start.elapsed()
            );
        }
        sleep(Duration::from_millis(50)).await;
    }
}

// ===========================================================================
// 1. install + list flow
// ===========================================================================

#[tokio::test]
async fn install_lists_and_details_round_trip() {
    let (state, _tmp, plugins_dir) = boot_state().await;
    // Source path lives OUTSIDE plugins_dir so install must materialize a
    // copy/link into plugins_dir/<id> — the realistic flow.
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.install");

    // POST install.
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({
            "source": { "kind": "local_path", "path": src_dir.to_string_lossy() }
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED, "install should 201");
    let body = body_to_json(resp).await;
    assert_eq!(body["id"], "test.install");
    assert_eq!(body["enabled"], false);
    assert_eq!(body["state"], "disabled");

    // The install path the host knows about should now exist as a symlink
    // (unix) or directory (windows) under plugins_dir.
    assert!(
        plugins_dir.join("test.install").exists(),
        "plugins_dir entry should exist"
    );

    // GET list.
    let resp = get_path(app(state.clone()), "/api/plugins").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list = body_to_json(resp).await;
    let arr = list.as_array().expect("list should be array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], "test.install");
    assert_eq!(arr[0]["manifest_name"], "Echo Stub");

    // GET detail.
    let resp = get_path(app(state.clone()), "/api/plugins/test.install").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let det = body_to_json(resp).await;
    assert_eq!(det["id"], "test.install");
    assert!(det["manifest"]["views"].is_array());
}

// ===========================================================================
// 2. enable spawns the process
// ===========================================================================

#[tokio::test]
async fn enable_transitions_to_running() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.enable");

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/test.enable/enable",
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "enable should 200");
    let det = body_to_json(resp).await;
    assert_eq!(det["enabled"], true);

    // The state can be `spawning` momentarily; poll until `running`.
    let det = wait_for_state(&state, "test.enable", "running", Duration::from_secs(3)).await;
    assert_eq!(det["enabled"], true);

    // Cleanup.
    let _ = post_json(
        app(state.clone()),
        "/api/plugins/test.enable/disable",
        json!({}),
    )
    .await;
}

// ===========================================================================
// 3. disable stops the process
// ===========================================================================

#[tokio::test]
async fn disable_transitions_to_disabled() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.disable");
    post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;
    post_json(
        app(state.clone()),
        "/api/plugins/test.disable/enable",
        json!({}),
    )
    .await;
    wait_for_state(&state, "test.disable", "running", Duration::from_secs(3)).await;

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/test.disable/disable",
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let det = body_to_json(resp).await;
    assert_eq!(det["enabled"], false);
    assert_eq!(det["state"], "disabled");
}

// ===========================================================================
// 4. log endpoint returns stderr from the running stub
// ===========================================================================

#[tokio::test]
async fn log_tail_returns_stub_stderr() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.log");
    post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;
    post_json(
        app(state.clone()),
        "/api/plugins/test.log/enable",
        json!({}),
    )
    .await;
    wait_for_state(&state, "test.log", "running", Duration::from_secs(3)).await;

    // Stub writes a startup line to stderr; the ring should pick it up.
    let resp = get_path(app(state.clone()), "/api/plugins/test.log/log?n=10").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let lines = body_to_json(resp).await;
    let arr = lines.as_array().expect("array");
    assert!(
        arr.iter()
            .any(|s| s.as_str().unwrap_or("").contains("stub-echo")),
        "expected stderr to contain stub line, got {:?}",
        arr
    );

    let _ = post_json(
        app(state.clone()),
        "/api/plugins/test.log/disable",
        json!({}),
    )
    .await;
}

// ===========================================================================
// 5. uninstall cascades tokens / kv / overlays
// ===========================================================================

#[tokio::test]
async fn uninstall_cascades_satellites() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.uninstall");
    post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;

    // Seed satellite data so we can verify the cascade.
    state
        .repo
        .plugin_kv_set("test.uninstall", "foo", &json!("bar"))
        .await
        .unwrap();
    state
        .raw_repo()
        .overlay_upsert(NewOverlay {
            plugin_id: "test.uninstall".into(),
            entity_kind: "track".into(),
            entity_id: "w1".into(),
            kind: "status".into(),
            payload: json!({"x": 1}),
        })
        .await
        .unwrap();

    let resp = delete_path(app(state.clone()), "/api/plugins/test.uninstall").await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Subsequent GET should 404.
    let resp = get_path(app(state.clone()), "/api/plugins/test.uninstall").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Token, kv, overlays — all gone.
    assert!(
        state
            .repo
            .plugin_token_get("test.uninstall")
            .await
            .unwrap()
            .is_none()
    );
    let kv = state
        .repo
        .plugin_kv_list("test.uninstall", "")
        .await
        .unwrap();
    assert!(kv.is_empty(), "kv should be empty after uninstall");
    let overlays = state.repo.overlays_for("track", "w1").await.unwrap();
    assert!(
        overlays.is_empty(),
        "overlays should be cleared on uninstall"
    );
}

// ===========================================================================
// 6. views catalog reflects the installed + enabled manifest
// ===========================================================================

#[tokio::test]
async fn views_catalog_lists_enabled_plugin_views() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.views");

    // Before install: empty catalog.
    let resp = get_path(app(state.clone()), "/api/plugins/views").await;
    let arr = body_to_json(resp).await;
    assert!(arr.as_array().unwrap().is_empty());

    post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;

    // Disabled plugin: still empty (only enabled plugins surface views).
    let resp = get_path(app(state.clone()), "/api/plugins/views").await;
    let arr = body_to_json(resp).await;
    assert!(
        arr.as_array().unwrap().is_empty(),
        "disabled plugin should not surface views"
    );

    post_json(
        app(state.clone()),
        "/api/plugins/test.views/enable",
        json!({}),
    )
    .await;
    wait_for_state(&state, "test.views", "running", Duration::from_secs(3)).await;

    let resp = get_path(app(state.clone()), "/api/plugins/views").await;
    let arr = body_to_json(resp).await;
    let entries = arr.as_array().expect("array");
    assert_eq!(entries.len(), 1);
    // `resource_uri` is the canonical MCP Apps identifier; the frontend
    // parses (plugin_id, view_id) off it lazily.
    assert_eq!(entries[0]["resource_uri"], "ui://test.views/main");
    assert_eq!(entries[0]["scope"], "card");
    assert_eq!(entries[0]["default_size"]["w"], 4);
    // Legacy `plugin_id` / `view_id` fields were dropped pre-prod (#89).
    assert!(entries[0].get("plugin_id").is_none());
    assert!(entries[0].get("view_id").is_none());

    let _ = post_json(
        app(state.clone()),
        "/api/plugins/test.views/disable",
        json!({}),
    )
    .await;
}

// ===========================================================================
// 7. install rejects manifest with disallowed `scope: "track"`
// ===========================================================================

#[tokio::test]
async fn install_rejects_track_scope_manifest() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_bad_scope_plugin(src_root.path(), "test.badscope");

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_to_text(resp).await;
    assert!(
        body.contains("scope") || body.contains("track"),
        "error should mention scope/track, got {body}"
    );
}

// ===========================================================================
// 8. install rejects reinstall with 409
// ===========================================================================

#[tokio::test]
async fn install_twice_returns_409() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.dup");

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = body_to_text(resp).await;
    assert!(body.contains("already installed"), "got: {body}");
}

// ===========================================================================
// Bonus: install rejects unsupported source kind with 400.
// ===========================================================================

#[tokio::test]
async fn install_rejects_unsupported_source() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let resp = post_json(
        app(state),
        "/api/plugins/install",
        json!({ "source": { "kind": "tarball", "url": "https://example.com/x.tar" } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ===========================================================================
// #1284 S1 — `PATCH /api/plugins/{id}/config` is a real PATCH with a real
// validator, and the list can say whether a plugin has anything to configure.
//
// **One pre-existing assertion is overturned here.** The old
// `patch_config_writes_user_config` installed a stub with **no**
// `config_schema` and asserted the write returned 200 — that was the whole
// point of the old endpoint, which took `Json<Value>` and wrote it through
// unvalidated. §2.2.2 makes that case a 400: a plugin with no declared
// configurable surface has no key this endpoint could meaningfully store, and
// accepting arbitrary JSON was exactly how "configuration" stayed a field with
// no semantics. The test below keeps the name and the round-trip, but drives a
// plugin that *declares* a schema; `patch_config_on_a_plugin_without_a_schema_is_400`
// pins the overturned cell.
//
// **A second assertion is overturned by the S1 review**: the write no longer
// enforces `required` — see `patch_config_does_not_enforce_required_keys`,
// which carries the adjudication.
//
// -------------------------------------------------------------------------
// Mutation witness table (every row was applied to the tree and run; the
// "also red" column is not decoration — two of these rows are NOT orthogonal
// and saying so is the point).
//
// | # | mutation | red test | red assertion |
// |---|---|---|---|
// | 1 | `patch` + `has_config` read `config_schema` out of the persisted blob again, registry **gate left in place** | `a_manifest_blob_written_by_an_older_kernel_still_has_a_config_surface` **and** `a_row_whose_manifest_is_not_in_the_registry_is_refused_explicitly` | `has_config` `left: false / right: true`; and `left: 200 / right: 409`. NOT orthogonal, by construction: one mutation restores one read source, and both witnesses are about that source. **Re-run in round 3** (P2-6) against the current tree: the observed red set is unchanged at two. `a_registry_gap_answers_409_even_for_a_plugin_that_declares_no_schema`, which did not exist at `46dc4e68`, stays **green** — the schema *source* moves, the registry *gate* does not, so that plugin still gets its 409 |
// | 1b | the same, **plus** deleting the registry gate — i.e. the pre-review source in full | row 1's two **and** `a_registry_gap_answers_409_even_for_a_plugin_that_declares_no_schema` | `left: 400 / right: 409`. Run in round 3 to settle exactly which half of the round-1 mutation the round-2 test can see: it is the gate, not the read source. Overlaps row 21/22 on purpose — the seam has two independent halves and each needs its own witness |
// | 2 | delete the `reject_undeclared_keys` sweep over the request's keys | `patch_config_judges_key_names_before_null_means_delete` **and** `a_stored_key_the_schema_no_longer_declares_does_not_lock_the_operator_out`, `patch_config_rejects_values_that_violate_the_schema` | `left: 200 / right: 400` (`{"ghost": null}`). **Round-3 re-run: the round-1 entry under-reported this at one test.** The other two arrived later and see the same sweep: the narrow test's negative half (`{"old": "again"}` must still be refused) and the value-rejection test's undeclared-key row |
// | 3 | validate the merged map unpruned (drop the `judged` copy) | `a_stored_key_the_schema_no_longer_declares_does_not_lock_the_operator_out` | "an invisible key must not reject a legal request: … config.old: unknown field", `left: 400 / right: 200` |
// | 4 | stop stripping `required` before validating the stored map | `patch_config_does_not_enforce_required_keys` | "a partial Save must not be refused for the keys it deliberately omits", `left: 400 / right: 200`. **Re-run in round 3**: still exactly one |
// | 5 | restore `existing.user_config … unwrap_or_default()` | `patch_config_refuses_to_overwrite_a_non_object_user_config` | `left: 200 / right: 409` |
// | 6 | `registry_gap` returns `BadRequest` instead of a 409 | `a_row_whose_manifest_is_not_in_the_registry_is_refused_explicitly` **and** `a_registry_gap_answers_409_even_for_a_plugin_that_declares_no_schema` | `left: 400 / right: 409`. Not orthogonal: one refusal, two entry conditions (a plugin that declares a schema, and one that does not) |
// | 7 | `effective_config`: store a `null` instead of falling back (`if !value.is_null()` → `if true`) | `plugin_host::config::tests::a_stored_null_falls_back_to_the_default` | `left: Some(Null) / right: Some(String("dark"))` |
// | 8 | `effective_config`: a schema with no `properties` passes `user_config` through | `plugin_host::config::tests::a_schema_without_properties_declares_nothing` | "a schema with no properties declares no keys" |
// | 9 | `validate_instance`'s byte-cap arm hard-codes the `template_input` root | `plugin_host::template_input::tests::instance_violations_report_under_the_callers_root_path` **and** `patch_config_rejects_values_that_violate_the_schema` | root-path assertions. Not orthogonal — one shared `format!`, two callers. Before this round only the route test would have caught it; the unit test's byte-cap arm was asserted in a comment and nowhere else. |
// | 10 | non-object body falls back to an empty patch instead of 400 | `patch_config_rejects_a_non_object_body_and_accepts_an_empty_one` | `body ["not","an","object"]`, `left: 200 / right: 400`. **Re-run in round 3**: still exactly one |
// | 11 | a plugin with no `config_schema` gets an empty schema instead of a 400 | `patch_config_on_a_plugin_without_a_schema_is_400_even_for_an_empty_body` **and** `patch_config_on_a_plugin_without_a_schema_is_400` | `body {}`, `left: 200 / right: 400`. Not orthogonal: the two tests are the same rule at two body shapes, which is why the second one exists. **Re-run in round 3**: still exactly two |
//
// Round 2 additions and re-judgements. **Which rows were actually re-run
// matters more than the table looking uniform**, so: rows 3, 5 and 6 were
// touched by round 2, were re-applied to this tree and are shown in their new
// observed form (row 5's `left` was 200 → the refusal is now 409, not 500).
//
// **Round 3 (P2-6) corrects how carry-forward was justified here.** Round 2
// carried rows 1, 2, 4 and 7–11 with the reason "round 2 did not move the code
// they mutate". That is only half of what carry-forward needs: a red set can
// also grow because a *new test* can see an old mutation, with the mutated
// code untouched. Both conditions are required —
//
//   (i) the mutated code has not changed since the run, **and**
//   (ii) no test added since then can observe that mutation.
//
// — and (ii) is the one that failed. Rows 1, 2, 4, 10 and 11 (every carried
// row whose mutation is observable over HTTP) were therefore re-applied and
// re-run against the current tree. **Row 2 was under-reported**: one test in
// round 1, three observed now. Row 1's red set is unchanged, and the round-3
// review's expectation that it would also redden the new registry-gap test is
// **not** what the tree does — row 1b isolates why. Rows 4, 10, 11 are
// unchanged at their recorded counts.
//
// Rows 7, 8, 9 and 24 stay carried: they mutate `plugin_host::config` /
// `plugin_host::template_input`, round 3 added no test in either module, and
// the route-level tests it did add assert nothing those mutations move
// (the new cap message is the route's own, not `validate_instance`'s).
//
// Rows 12–24 below were each applied to this tree, run, and restored; the
// "red test" column is the observed set, not the intended one. Two of them
// (16 and 19) turned out to be **less** orthogonal than expected, and that is
// recorded rather than papered over.
//
// | # | mutation | red test | red assertion |
// |---|---|---|---|
// | 12 | **P0-B**, the round-1 shape restored: prune `merged` itself and store the pruned map | `a_stored_key_the_schema_no_longer_declares_does_not_lock_the_operator_out` | "the write unlocked, and it did NOT delete the key the operator never touched", `left: {"keep":"b"} / right: {"keep":"b","old":"residue"}` — and the widen-back leg, `left: Null / right: "residue"` |
// | 13 | **P0-C**, the round-1 shape restored: `CalmError::Internal` for a non-object `user_config` | `patch_config_refuses_to_overwrite_a_non_object_user_config` | "a corrupt row is a state the operator can fix, not a server fault", `left: 500 / right: 409` |
// | 14 | ignore the `reset` flag (always merge into the stored value) | `patch_config_refuses_to_overwrite_a_non_object_user_config` **and** `reset_is_destructive_only_when_the_operator_asks_for_it` | "the named recovery action must actually recover", `left: 409 / right: 200`; and `left: {"theme":"light","label":"a"} / right: {}`. Not orthogonal on purpose: the escape hatch and its meaning on a healthy row are one flag |
// | 15 | `reset` is implicit — treat an empty patch `{}` as a reset | `reset_is_destructive_only_when_the_operator_asks_for_it` **and** `patch_config_rejects_a_non_object_body_and_accepts_an_empty_one` | "an empty Save must not be a reset"; and "an empty patch changes nothing". The single-violation companion to 14: without it, "reset works" is satisfiable by a reset that is always on. Both reds are the same sentence asserted from the two sides |
// | 16 | **P0-A**: `build_detail` publishes `plug.manifest["config_schema"]` (the blob) instead of the registry's | `a_manifest_blob_written_by_an_older_kernel_still_has_a_config_surface` **and** `a_row_whose_manifest_is_not_in_the_registry_is_refused_explicitly` | "the form's schema comes from the registry, like every other config answer in this response", `left: Null / right: {…}`; and "and no schema is published either — the three config answers agree". Not orthogonal, observed: the blob outlives the registry entry, so a blob-sourced field also contradicts the registry-gap reader |
// | 17 | **P1-D**: `registry_gap` raises the generic `CalmError::Conflict` | `a_row_whose_manifest_is_not_in_the_registry_is_refused_explicitly` **and** `a_registry_gap_answers_409_even_for_a_plugin_that_declares_no_schema` | `left: "conflict" / right: "plugin_manifest_unloaded"`. The status stays 409 under this mutation, which is the point: the round-1 test could not see it, because it matched on message text |
// | 18 | drop "manifest.json" from the `registry_gap` message | `a_row_whose_manifest_is_not_in_the_registry_is_refused_explicitly` | "the durable cause needs naming, not just the transient one" |
// | 19 | **P1-F**, the "cap the request body" implementation: validate `patch` instead of the merged document | `the_byte_cap_is_measured_on_the_merged_config_not_the_request_body` **and** `patch_config_leaves_absent_keys_alone_and_deletes_on_explicit_null`, `patch_config_judges_key_names_before_null_means_delete`, `patch_config_does_not_enforce_required_keys` | "the cap is on the merged storage state, and 5000 + 5000 > 8192", `left: 200 / right: 400`. The other three are collateral (validating the patch also stops validating the merge, so `null`-deletes stop being checked at all) — the row that is *only* about the cap is the first one. The round-1 witness, a single 9000-byte request, stays **green** under this mutation: that is precisely why it was not evidence for §2.2.1 |
// | 20 | the cap is a precondition on the stored row (validate before merging) | `the_byte_cap_is_measured_on_the_merged_config_not_the_request_body` | "an oversized row must be shrinkable through the API", `left: 400 / right: 200`. Pairs with 19: one direction alone is satisfied by an implementation that locks the operator out |
// | 21 | **order**: registry lookup moved below the `config_schema` read (schema taken from the blob) | `a_registry_gap_answers_409_even_for_a_plugin_that_declares_no_schema` **and** `a_manifest_blob_written_by_an_older_kernel_still_has_a_config_surface` | "the state gate has to run before the schema gate", `left: 400 / right: 409`; and "an upgraded install must not need a manual reload". The second red is the same reason the order exists — a schema gate that runs on the blob is a gate on a document the kernel did not read |
// | 22 | **order**: the row lookup moved below the registry lookup | `patch_config_unknown_id_is_still_404` **and** the ghost leg of `a_registry_gap_answers_409_even_for_a_plugin_that_declares_no_schema` | `left: 409 / right: 404`. Not orthogonal: `404 → 409` is one edge, asserted at both ends |
// | 23 | **P2-G**: `declares_key` returns `true` unconditionally | five, in two layers: `plugin_host::template_input::tests::{declares_key_answers_the_membership_question_directly, an_undeclared_key_reports_the_same_shipped_string_at_both_entry_points, reject_undeclared_keys_refuses_everything_when_properties_is_absent}` **and** `patch_config_judges_key_names_before_null_means_delete`, `a_stored_key_the_schema_no_longer_declares_does_not_lock_the_operator_out` | `assert!(!declares_key(&schema(), "ghost"))`; `left: 200 / right: 400`. Deliberately not orthogonal — the point of the extraction is that one predicate now serves both the verdict and the filter, so one mutation has to reach both |
// | 24 | **P1-E**: `missing_required` returns an empty vec | `plugin_host::config::tests::missing_required_names_only_the_keys_nothing_supplies` | `left: [] / right: ["token", "secondary"]`. Unit-only by construction — S1 owns the seam, S2/S3a/S3b own the consumers that will make it observable over HTTP |
//
// Round 3 additions (rows 25–27, plus 1b above). Each was applied to this
// tree, run over the whole `test(plugin) or test(config) or test(template_input)`
// selection with `--no-fail-fast` (490 tests), and restored; the red column is
// the observed set.
//
// | # | mutation | red test | red assertion |
// |---|---|---|---|
// | 25 | **P1-1**: drop the total cap on the stored document (`if false && stored_bytes > …`) | `residue_cannot_grow_the_stored_config_without_bound` | panics on `refusal.expect("the row must stop growing at some point")` — the narrow/fill loop runs out of rounds with every write still a 200. Exactly one red: the per-write cap keeps every other test green under this mutation, which is the whole point of the row |
// | 25b | the total cap becomes a **precondition on the row the request found** (measure `existing.user_config`, not the document being stored) | `residue_cannot_grow_the_stored_config_without_bound` | `left: 400 / right: 200` on "the named recovery action must actually recover" — the refusal still fires at the right time, but `?reset=true` is refused too and the row becomes unshrinkable. Pairs with 25 exactly as 20 pairs with 19: a cap witnessed in one direction only is satisfiable by a lockout |
// | 26 | **P1-2**: delete the `try_lock_lifecycle` guard from the handler (the pre-round-3 shape) | `a_config_write_refuses_while_another_lifecycle_operation_holds_the_plugin` | `left: 200 / right: 409` — the write goes through while another lifecycle operation holds the plugin. Exactly one red, and `plugin_lifecycle_lock::a12a_every_pair_of_entry_points_settles` is **not** in it: that suite enumerates the entry points that took the lock before this round, so the config write's guard needed a witness of its own |
// | 27 | **P2-3**: restore `user_config = excluded.user_config` in `plugin_install`'s `ON CONFLICT DO UPDATE` | `repo::plugin_install_upsert_never_resets_operator_config` | `left: {} / right: {"theme":"light"}`. In a different crate's SQL from every other row here, which is the point of P2-3: the sentence "PATCH is the only writer of `user_config`" is now falsified by a failing test rather than by re-reading `install`'s duplicate-id check |
//
// **P2-4 has no row, and that is a statement rather than an omission.** The
// `warn!` on `?reset=true` has no reader in this tree — no assertion, no
// consumer — so any "witness" for it would be a test that re-implements the
// log line. It is there for an operator reading kernel logs after the fact;
// the write it accompanies is witnessed by rows 14 and 15 above.
// ===========================================================================

/// The config schema every test in this section configures against. All keys
/// optional (so it is legal at `manifest_version: 2`), one with a `default` so
/// the read-time merge has something to do.
fn stub_config_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "theme": { "type": "string", "enum": ["dark", "light"], "default": "dark" },
            "retries": { "type": "integer" },
            "label": { "type": "string" }
        },
        "additionalProperties": false
    })
}

/// `write_stub_plugin`, plus a `config_schema` on the manifest. Written by
/// editing the manifest file the shared fixture just produced, so the two can
/// not drift — and it still goes through the real install route, i.e. through
/// `Manifest::parse`, so an invalid schema here would fail the install rather
/// than quietly reaching the route.
fn write_stub_plugin_with_config(plugins_dir: &Path, id: &str, config_schema: Value) -> PathBuf {
    let plugin_dir = write_stub_plugin(plugins_dir, id);
    let path = plugin_dir.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    manifest["config_schema"] = config_schema;
    std::fs::write(&path, serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
    plugin_dir
}

async fn install(state: &AppState, src_dir: &Path) {
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "install failed: {}",
        body_to_text(resp).await
    );
}

async fn patch_config(state: &AppState, id: &str, body: Value) -> axum::http::Response<Body> {
    patch_config_query(state, id, "", body).await
}

/// Same, with a raw query string (`"?reset=true"`).
async fn patch_config_query(
    state: &AppState,
    id: &str,
    query: &str,
    body: Value,
) -> axum::http::Response<Body> {
    app(state.clone())
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/plugins/{id}/config{query}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn patch_config_writes_user_config() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir =
        write_stub_plugin_with_config(src_root.path(), "test.config", stub_config_schema());
    install(&state, &src_dir).await;

    let resp = patch_config(&state, "test.config", json!({ "theme": "light" })).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let det = body_to_json(resp).await;
    assert_eq!(det["user_config"]["theme"], "light");
    // …and the effective view carries the same value, not the default.
    assert_eq!(det["effective_config"]["theme"], "light");
}

/// The overturned cell. Pre-#1284 this exact request was a 200.
#[tokio::test]
async fn patch_config_on_a_plugin_without_a_schema_is_400() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.noschema");
    install(&state, &src_dir).await;

    let resp = patch_config(&state, "test.noschema", json!({ "theme": "dark" })).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_to_text(resp).await;
    assert!(body.contains("config_schema"), "got: {body}");

    // And nothing was written — the refusal is not a "wrote it anyway, then
    // complained".
    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/test.noschema").await).await;
    assert_eq!(det["user_config"], json!({}), "got {det:?}");
}

/// 404 behaviour is explicitly unchanged, and precedes the 400: an unknown id
/// is an unknown id whatever the body says. Without this, moving the
/// schema check above the existence probe would leak "this plugin has no
/// config schema" for plugins that do not exist.
#[tokio::test]
async fn patch_config_unknown_id_is_still_404() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let resp = patch_config(&state, GHOST, json!({ "theme": "dark" })).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// §2.2.3 — the three patch cells, on one plugin, in sequence, because the
/// semantics are about *accumulated* state and a fresh row would prove nothing.
#[tokio::test]
async fn patch_config_leaves_absent_keys_alone_and_deletes_on_explicit_null() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir =
        write_stub_plugin_with_config(src_root.path(), "test.patch", stub_config_schema());
    install(&state, &src_dir).await;

    // Set two keys.
    let det = body_to_json(
        patch_config(
            &state,
            "test.patch",
            json!({ "theme": "light", "label": "a" }),
        )
        .await,
    )
    .await;
    assert_eq!(
        det["user_config"],
        json!({ "theme": "light", "label": "a" })
    );

    // (1) Absent keys are untouched: this body names only `label`.
    let det = body_to_json(patch_config(&state, "test.patch", json!({ "label": "b" })).await).await;
    assert_eq!(
        det["user_config"],
        json!({ "theme": "light", "label": "b" }),
        "an absent key must keep its stored value, not be dropped"
    );

    // (2) Explicit null deletes the key…
    let det =
        body_to_json(patch_config(&state, "test.patch", json!({ "theme": null })).await).await;
    assert_eq!(det["user_config"], json!({ "label": "b" }));
    // …and the manifest default is in force again for it.
    assert_eq!(
        det["effective_config"]["theme"], "dark",
        "a cleared key falls back to its default, not to absent"
    );

    // (3) Deleting a key that has no default leaves it absent from effective.
    let det =
        body_to_json(patch_config(&state, "test.patch", json!({ "label": null })).await).await;
    assert_eq!(det["user_config"], json!({}));
    assert!(
        det["effective_config"].get("label").is_none(),
        "got {:?}",
        det["effective_config"]
    );
}

/// §2.2.1 — the instance validator, one row per rejection class, each pinned
/// by the field path it names.
#[tokio::test]
async fn patch_config_rejects_values_that_violate_the_schema() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin_with_config(src_root.path(), "test.bad", stub_config_schema());
    install(&state, &src_dir).await;

    let cases = [
        (
            "wrong type",
            json!({ "retries": "three" }),
            "config.retries",
        ),
        ("outside enum", json!({ "theme": "neon" }), "config.theme"),
        ("undeclared key", json!({ "ghost": "x" }), "config.ghost"),
        (
            "oversized value",
            json!({ "label": "x".repeat(9000) }),
            "config",
        ),
    ];
    for (label, body, expected_path) in cases {
        let resp = patch_config(&state, "test.bad", body).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{label}");
        let text = body_to_text(resp).await;
        assert!(
            text.contains(expected_path),
            "{label}: expected `{expected_path}` in the error, got: {text}"
        );
        // Errors must name the CONFIG field, never the track-input one whose
        // validator this reuses (#1284 §2.1 / F8).
        assert!(
            !text.contains("template_input") && !text.contains("input_schema"),
            "{label}: error leaked the other root path: {text}"
        );
    }

    // Positive control: the same plugin accepts a conforming body, so the
    // rows above are rejections of the value and not of the endpoint.
    let resp = patch_config(
        &state,
        "test.bad",
        json!({ "retries": 3, "theme": "light" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // And a rejection leaves the previous value standing.
    let resp = patch_config(&state, "test.bad", json!({ "theme": "neon" })).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/test.bad").await).await;
    assert_eq!(det["user_config"]["theme"], "light");
}

/// **P1-F.** §2.2.1 says the byte cap governs the **merged storage state**,
/// not the request body — and the only witness for it sent a single 9000-byte
/// request, which a cap on the request body passes just as well. That witness
/// could not tell the two implementations apart, so it was not evidence for
/// the sentence it was cited for.
///
/// The discriminating shape: two patches of ~5000 bytes each on *different*
/// keys. Each request is comfortably under 8192; their merge is not. A
/// request-body cap accepts both.
///
/// Paired with the reverse, which is what keeps the cap from being a lockout
/// (branch table cell 12): a row that is *already* over the cap must still
/// accept a patch that shrinks it, because the check is on the resulting
/// document rather than a precondition on the row.
#[tokio::test]
async fn the_byte_cap_is_measured_on_the_merged_config_not_the_request_body() {
    let (state, _tmp, _plugins_dir, repo) = boot_state_with_repo().await;
    let src_root = tempfile::tempdir().unwrap();
    let two_strings = json!({
        "type": "object",
        "properties": {
            "a": { "type": "string" },
            "b": { "type": "string" }
        },
        "additionalProperties": false
    });
    let src_dir = write_stub_plugin_with_config(src_root.path(), "test.cap", two_strings);
    install(&state, &src_dir).await;

    let chunk = "x".repeat(5000);

    // First half: ~5000 bytes, well inside the 8192 cap. Accepted.
    let resp = patch_config(&state, "test.cap", json!({ "a": chunk })).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "5000 bytes is under the cap: {}",
        body_to_text(resp).await
    );

    // Second half: another ~5000 bytes, on a *different* key, so the request
    // is again under the cap while the merge is over it. This is the request a
    // request-body cap would wave through.
    let resp = patch_config(&state, "test.cap", json!({ "b": chunk.clone() })).await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "the cap is on the merged storage state, and 5000 + 5000 > 8192"
    );
    let text = body_to_text(resp).await;
    assert!(text.contains("8192"), "got: {text}");

    // …and the refusal left the row as it was.
    let row = repo.plugin_get_by_id("test.cap").await.unwrap().unwrap();
    assert_eq!(row.user_config.as_object().unwrap().len(), 1);

    // Reverse: a row already over the cap (only a direct write can produce
    // one) still accepts a patch that makes it smaller. If the cap were a
    // precondition on the stored row rather than a check on the result, the
    // operator would be stuck with a row no API could shrink.
    repo.plugin_update_user_config(
        "test.cap",
        json!({ "a": chunk.clone(), "b": chunk.clone() }),
    )
    .await
    .unwrap();
    let resp = patch_config(&state, "test.cap", json!({ "b": "small" })).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "an oversized row must be shrinkable through the API: {}",
        body_to_text(resp).await
    );
    let det = body_to_json(resp).await;
    assert_eq!(det["user_config"]["b"], "small");
    assert_eq!(
        det["user_config"]["a"], chunk,
        "and the untouched key kept its value"
    );

    // Deleting the big key outright is likewise a way down.
    let det = body_to_json(patch_config(&state, "test.cap", json!({ "a": null })).await).await;
    assert_eq!(det["user_config"], json!({ "b": "small" }));
}

/// **Overturned by the S1 review.** This test was
/// `patch_config_enforces_required_keys_but_lets_defaults_satisfy_them`, and
/// it pinned a rule that cannot coexist with §2.2.5: if a Save carries only
/// the keys the operator edited, then a plugin with two no-default `required`
/// keys can never make a first Save that validates — every one of them is
/// missing something. The adjudication is that the **write** does not enforce
/// `required`; **consumption** does (S2/S3 bring-up), where a plugin missing
/// required configuration fails to start into the `unavailable` + `last_error`
/// terminal state §2.4 already defines.
///
/// The schema here still needs `manifest_version: 3` (§2.1), which is the only
/// place in this suite that version is exercised end to end through install.
#[tokio::test]
async fn patch_config_does_not_enforce_required_keys() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin_with_config(
        src_root.path(),
        "test.required",
        json!({
            "type": "object",
            "properties": {
                "token": { "type": "string" },
                "secondary": { "type": "string" },
                "region": { "type": "string", "default": "eu" }
            },
            "required": ["token", "secondary", "region"],
            "additionalProperties": false
        }),
    );
    // …and the manifest has to say 3 for that schema to install at all.
    let path = src_dir.join("manifest.json");
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    m["manifest_version"] = json!(3);
    std::fs::write(&path, serde_json::to_string_pretty(&m).unwrap()).unwrap();
    install(&state, &src_dir).await;

    // The case the old rule made impossible: two required keys with no
    // defaults, and a form that submits one field at a time.
    let resp = patch_config(&state, "test.required", json!({ "token": "t" })).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a partial Save must not be refused for the keys it deliberately omits"
    );
    let det = body_to_json(resp).await;
    assert_eq!(
        det["user_config"],
        json!({ "token": "t" }),
        "no default stored"
    );
    assert_eq!(det["effective_config"]["region"], "eu");

    // …and the second field lands on top of the first rather than replacing it.
    let det =
        body_to_json(patch_config(&state, "test.required", json!({ "secondary": "s" })).await)
            .await;
    assert_eq!(
        det["user_config"],
        json!({ "token": "t", "secondary": "s" })
    );

    // Clearing a required key is likewise allowed at the write — the plugin
    // simply will not come up until it is set again.
    let resp = patch_config(&state, "test.required", json!({ "token": null })).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "required is enforced at bring-up, not here"
    );
    let det = body_to_json(resp).await;
    assert_eq!(det["user_config"], json!({ "secondary": "s" }));
    assert!(
        det["effective_config"].get("token").is_none(),
        "and it really is gone from what would be in force: {det:?}"
    );

    // The negative half: dropping `required` from the write path must not have
    // dropped the *other* constraints along with it. Same plugin, same schema.
    let resp = patch_config(&state, "test.required", json!({ "token": 7 })).await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "types are still enforced"
    );
}

/// **P0-2.** `{"ghost": null}` used to return 200: the merge loop read `null`
/// as "delete this key" and removed it *before* validation, so the key never
/// reached the validator and "the request is validated against the schema" was
/// false for exactly the shape an attacker or a buggy client would send.
///
/// Paired, because a blanket refusal of `null` would also pass the first half:
/// a **declared** key with `null` must still delete.
#[tokio::test]
async fn patch_config_judges_key_names_before_null_means_delete() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir =
        write_stub_plugin_with_config(src_root.path(), "test.ghostnull", stub_config_schema());
    install(&state, &src_dir).await;

    // negative: undeclared key, null value ⇒ still refused, and named.
    let resp = patch_config(&state, "test.ghostnull", json!({ "ghost": null })).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let text = body_to_text(resp).await;
    assert!(text.contains("config.ghost"), "got: {text}");

    // …including when it rides along with a perfectly good key, which must not
    // be written either.
    let resp = patch_config(
        &state,
        "test.ghostnull",
        json!({ "label": "keep", "ghost": null }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/test.ghostnull").await).await;
    assert_eq!(det["user_config"], json!({}), "nothing was written");

    // positive: a declared key with `null` still deletes.
    let det =
        body_to_json(patch_config(&state, "test.ghostnull", json!({ "label": "x" })).await).await;
    assert_eq!(det["user_config"], json!({ "label": "x" }));
    let det =
        body_to_json(patch_config(&state, "test.ghostnull", json!({ "label": null })).await).await;
    assert_eq!(det["user_config"], json!({}));
}

/// **P0-3, re-judged in round 2 (P0-B).** The operator must not be lockable
/// out by a key no UI shows — *and* unlocking them must not cost them data.
///
/// After a schema drops key `old`, the stored row still holds it. The original
/// cut merged the stored map and validated the result, so `old` — invisible in
/// the form, already ignored by `effective_config` — made **every** subsequent
/// PATCH 400 with "unknown field `old`", however legal the request, until the
/// operator guessed to send `{"old": null}` for a key they cannot see.
///
/// Round 1 fixed that by pruning the stored map **and writing the pruned map
/// back**, and this test asserted the deletion. That is a silent destructive
/// write on a path whose own comment said "a write path must never be the
/// thing that loses configuration": editing `keep` deleted the operator's
/// `old`, which they never asked to delete and could not see was at risk.
/// Round 2 validates a *pruned copy* and stores the unpruned map, so the
/// assertion below flips: the residue **survives**, it stays out of
/// `effective_config` (nothing runs with it), and the unlock is unchanged.
#[tokio::test]
async fn a_stored_key_the_schema_no_longer_declares_does_not_lock_the_operator_out() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let wide = json!({
        "type": "object",
        "properties": {
            "keep": { "type": "string" },
            "old": { "type": "string" }
        },
        "additionalProperties": false
    });
    let src_dir = write_stub_plugin_with_config(src_root.path(), "test.narrow", wide);
    install(&state, &src_dir).await;

    let det = body_to_json(
        patch_config(
            &state,
            "test.narrow",
            json!({ "keep": "a", "old": "residue" }),
        )
        .await,
    )
    .await;
    assert_eq!(det["user_config"], json!({ "keep": "a", "old": "residue" }));

    // The manifest narrows: `old` is gone. Reload is how a new manifest
    // reaches the kernel (§2.4), and it rewrites both the registry and the
    // published blob.
    let path = src_dir.join("manifest.json");
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    m["config_schema"] = json!({
        "type": "object",
        "properties": { "keep": { "type": "string" } },
        "additionalProperties": false
    });
    std::fs::write(&path, serde_json::to_string_pretty(&m).unwrap()).unwrap();
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/test.narrow/reload",
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "reload failed");

    // The residue is still in the row…
    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/test.narrow").await).await;
    assert_eq!(det["user_config"]["old"], "residue");
    assert!(
        det["effective_config"].get("old").is_none(),
        "…but nothing runs with it: {det:?}"
    );

    // …and a perfectly ordinary edit of a declared key still succeeds.
    let resp = patch_config(&state, "test.narrow", json!({ "keep": "b" })).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "an invisible key must not reject a legal request: {}",
        body_to_text(resp).await
    );
    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/test.narrow").await).await;
    assert_eq!(
        det["user_config"],
        json!({ "keep": "b", "old": "residue" }),
        "the write unlocked, and it did NOT delete the key the operator never touched"
    );
    assert!(
        det["effective_config"].get("old").is_none(),
        "…and the survivor still runs with nothing: {det:?}"
    );

    // And the value comes back if the manifest ever widens again — the reason
    // keeping it is worth something, not merely harmless.
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    m["config_schema"] = json!({
        "type": "object",
        "properties": {
            "keep": { "type": "string" },
            "old": { "type": "string" }
        },
        "additionalProperties": false
    });
    std::fs::write(&path, serde_json::to_string_pretty(&m).unwrap()).unwrap();
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/test.narrow/reload",
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "re-widening reload failed");
    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/test.narrow").await).await;
    assert_eq!(
        det["effective_config"]["old"], "residue",
        "the operator's value survived the narrow/widen round trip: {det:?}"
    );

    // Narrow it back for the negative half below.
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    m["config_schema"] = json!({
        "type": "object",
        "properties": { "keep": { "type": "string" } },
        "additionalProperties": false
    });
    std::fs::write(&path, serde_json::to_string_pretty(&m).unwrap()).unwrap();
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/test.narrow/reload",
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "re-narrowing reload failed");

    // Negative half: excluding residue from the judgement is not "accept
    // anything" — an undeclared key in the *request* is still refused,
    // including the one sitting in the row.
    let resp = patch_config(&state, "test.narrow", json!({ "old": "again" })).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_to_text(resp).await.contains("config.old"),
        "the pruned key may not be re-set"
    );
}

/// **P0-1.** The upgrade path, which is what makes the registry the only read
/// source non-negotiable rather than a matter of taste.
///
/// A plugin installed by a pre-#1284 kernel has a `plugins.manifest` blob with
/// **no** `config_schema` key: that kernel's `Manifest` had no such field and
/// serde dropped it on the way in. Reading the schema from the blob therefore
/// meant every already-installed plugin came up with no config surface — no
/// form (`has_config: false`) and a PATCH that 400s as "declares no
/// `config_schema`" — until an operator manually reloaded it. The registry,
/// which boot rebuilds from disk, has the schema all along.
#[tokio::test]
async fn a_manifest_blob_written_by_an_older_kernel_still_has_a_config_surface() {
    let (state, _tmp, _plugins_dir, repo) = boot_state_with_repo().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir =
        write_stub_plugin_with_config(src_root.path(), "test.upgrade", stub_config_schema());
    install(&state, &src_dir).await;

    // Rewrite the persisted blob to what an older kernel would have stored.
    let row = repo
        .plugin_get_by_id("test.upgrade")
        .await
        .unwrap()
        .unwrap();
    let mut blob = row.manifest.clone();
    assert!(
        blob.as_object_mut()
            .unwrap()
            .remove("config_schema")
            .is_some(),
        "fixture precondition: the blob carried the schema"
    );
    repo.plugin_update_manifest("test.upgrade", blob)
        .await
        .unwrap();

    // The list still offers the form…
    let rows = body_to_json(get_path(app(state.clone()), "/api/plugins").await).await;
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "test.upgrade")
        .unwrap()
        .clone();
    assert_eq!(row["has_config"], json!(true), "got {row:?}");

    // …the detail still folds in the manifest default…
    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/test.upgrade").await).await;
    assert_eq!(det["effective_config"], json!({ "theme": "dark" }));
    assert!(
        det["manifest"].get("config_schema").is_none(),
        "and the published blob really is the old one"
    );

    // **P0-A.** …and the schema §2.5 renders controls from is published too,
    // from the registry. Round 1 moved `has_config` and `effective_config` to
    // the registry and left this one on the blob, so this very test pinned the
    // contradiction: the API said "configurable" and "here is what is in
    // force" while being unable to hand a form the document to draw. The
    // assertion below is the half that used to be missing.
    assert_eq!(
        det["config_schema"],
        stub_config_schema(),
        "the form's schema comes from the registry, like every other config \
         answer in this response: {det:?}"
    );

    // …and the write is accepted and validated.
    let resp = patch_config(&state, "test.upgrade", json!({ "theme": "light" })).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "an upgraded install must not need a manual reload: {}",
        body_to_text(resp).await
    );
    let resp = patch_config(&state, "test.upgrade", json!({ "theme": "neon" })).await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "…and it is the registry's schema doing the validating"
    );
}

/// **P0-1, the other side of the same seam.** The row can exist while the
/// registry does not hold the manifest: during install (the row is written
/// before the registry insert) and, durably, when a plugin's `manifest.json`
/// fails to parse at boot — `registry::load_from_dir` skips it with a `warn!`
/// and leaves the row behind.
///
/// That window gets an explicit answer rather than a silent one: readers say
/// "nothing configurable", and the **writer** refuses with 409 and a distinct
/// message, never the 400 that means "this plugin will never have config".
#[tokio::test]
async fn a_row_whose_manifest_is_not_in_the_registry_is_refused_explicitly() {
    use axum::extract::FromRef;

    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin_with_config(src_root.path(), "test.gap", stub_config_schema());
    install(&state, &src_dir).await;

    // Reproduce the window: drop the registry entry, keep the row.
    let cs = calm_server::state::CodexShellState::from_ref(&state);
    let guard = cs.plugin.try_lock_lifecycle("test.gap").expect("lock free");
    assert!(cs.plugin.registry_remove(&guard).is_some());
    drop(guard);

    let resp = patch_config(&state, "test.gap", json!({ "theme": "light" })).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = body_to_json(resp).await;
    // **P1-D.** Round 1 raised the generic `Conflict`, whose code is
    // `"conflict"`, so the only thing left to distinguish this 409 from the
    // route's others was `text.contains("not loaded")` — the exact shape
    // `error.rs` forbids ("the distinction lives in the error code, not in the
    // message text") and the exact shape this assertion used to have.
    assert_eq!(
        body["code"], "plugin_manifest_unloaded",
        "409s are told apart by code: {body}"
    );
    let text = body.to_string();
    assert!(
        !text.contains("declares no"),
        "must not read as the permanent 'no schema' refusal: {text}"
    );
    // The durable half of this window is a `manifest.json` that fails to
    // parse, for which "reload the plugin" fails again — so the message has to
    // name the other action too, or the operator loops on the reload.
    assert!(
        text.contains("manifest.json"),
        "the durable cause needs naming, not just the transient one: {text}"
    );

    // And the readers agree, without inventing a schema.
    let rows = body_to_json(get_path(app(state.clone()), "/api/plugins").await).await;
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "test.gap")
        .unwrap()
        .clone();
    assert_eq!(row["has_config"], json!(false), "got {row:?}");
    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/test.gap").await).await;
    assert_eq!(det["effective_config"], json!({}));
    assert!(
        det.get("config_schema").is_none(),
        "and no schema is published either — the three config answers agree: {det:?}"
    );
    assert_eq!(det["user_config"], json!({}), "and nothing was written");
}

/// The refusal priority `404 → 409 → 400`, at the one cell that decides it:
/// a plugin that declares **no** `config_schema` *and* is missing from the
/// registry.
///
/// The kernel has not read that manifest, so it does not know the plugin
/// declares nothing — answering 400 ("this plugin will never be configurable")
/// would be asserting the contents of a document it never opened, and would
/// send the operator away instead of to a reload. 409 is the honest answer,
/// and it is what the gate order buys.
///
/// The other half of the order is `patch_config_unknown_id_is_still_404`: a
/// ghost id must 404 even though the registry has no manifest for it either.
#[tokio::test]
async fn a_registry_gap_answers_409_even_for_a_plugin_that_declares_no_schema() {
    use axum::extract::FromRef;

    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.gapnoschema");
    install(&state, &src_dir).await;

    // Positive control: while the registry holds it, the kernel *can* see that
    // it declares nothing, and says so permanently.
    let resp = patch_config(&state, "test.gapnoschema", json!({ "x": 1 })).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let cs = calm_server::state::CodexShellState::from_ref(&state);
    let guard = cs
        .plugin
        .try_lock_lifecycle("test.gapnoschema")
        .expect("lock free");
    assert!(cs.plugin.registry_remove(&guard).is_some());
    drop(guard);

    // Same plugin, same body — but now the kernel has no manifest to base
    // "declares no config_schema" on.
    let resp = patch_config(&state, "test.gapnoschema", json!({ "x": 1 })).await;
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "the state gate has to run before the schema gate"
    );
    let body = body_to_json(resp).await;
    assert_eq!(body["code"], "plugin_manifest_unloaded", "got {body}");

    // And 404 still outranks both: a ghost id is not "manifest unloaded".
    let resp = patch_config(&state, GHOST, json!({ "x": 1 })).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// **P1-5, re-judged in round 2 (P0-C).** A row whose `user_config` is not an
/// object is corrupt, not empty. `unwrap_or_default()` turned it into `{}` and
/// wrote the merge back on the next PATCH — silently discarding whatever it
/// held. Refusing is right; refusing with a **500 and no way out** was not.
///
/// This endpoint is the only writer of `user_config`, so round 1's
/// `CalmError::Internal` meant every subsequent request failed identically and
/// no API could restore the row: uninstall/reinstall or a hand-edited database
/// were the operator's only remedies. And the status was wrong on its own
/// terms — `error.rs` reserves `Internal` for "something went wrong
/// server-side", which nothing here did.
///
/// So: 409 with its own code, plus a real exit (`?reset=true`). The refusal
/// still does not echo the corrupt value — that part of round 1 stands.
#[tokio::test]
async fn patch_config_refuses_to_overwrite_a_non_object_user_config() {
    let (state, _tmp, _plugins_dir, repo) = boot_state_with_repo().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir =
        write_stub_plugin_with_config(src_root.path(), "test.corrupt", stub_config_schema());
    install(&state, &src_dir).await;
    repo.plugin_update_user_config("test.corrupt", json!("theme=light"))
        .await
        .unwrap();

    let resp = patch_config(&state, "test.corrupt", json!({ "theme": "light" })).await;
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "a corrupt row is a state the operator can fix, not a server fault"
    );
    let body = body_to_json(resp).await;
    assert_eq!(
        body["code"], "plugin_config_corrupt",
        "the distinction lives in the code, not the prose: {body}"
    );
    let text = body.to_string();
    assert!(text.contains("not a JSON object"), "got: {text}");
    assert!(
        text.contains("reset=true"),
        "the refusal must name the recovery action: {text}"
    );
    assert!(
        !text.contains("theme=light"),
        "and it must not echo the corrupt value back: {text}"
    );

    // The refusal is the point: the row is untouched, so an operator can still
    // recover what was there.
    let row = repo
        .plugin_get_by_id("test.corrupt")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.user_config, json!("theme=light"));

    // …and the way out really works, from the API, in one request.
    let resp = patch_config_query(
        &state,
        "test.corrupt",
        "?reset=true",
        json!({ "theme": "light" }),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "the named recovery action must actually recover: {}",
        body_to_text(resp).await
    );
    let det = body_to_json(resp).await;
    assert_eq!(det["user_config"], json!({ "theme": "light" }));

    // The plugin is fully usable again afterwards — the row is not merely
    // "different", it is back under the ordinary rules.
    let resp = patch_config(&state, "test.corrupt", json!({ "label": "x" })).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let det = body_to_json(resp).await;
    assert_eq!(
        det["user_config"],
        json!({ "theme": "light", "label": "x" })
    );
}

/// `?reset=true` is destructive by request, which is the only kind of
/// destruction this endpoint is allowed. Its meaning on a *healthy* row is
/// "put this plugin back on its manifest defaults", and it must not be
/// reachable by accident — the ordinary empty Save (`{}`, no query) is a
/// no-op, and that pair is asserted together on purpose.
#[tokio::test]
async fn reset_is_destructive_only_when_the_operator_asks_for_it() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir =
        write_stub_plugin_with_config(src_root.path(), "test.reset", stub_config_schema());
    install(&state, &src_dir).await;

    let det = body_to_json(
        patch_config(
            &state,
            "test.reset",
            json!({ "theme": "light", "label": "a" }),
        )
        .await,
    )
    .await;
    assert_eq!(
        det["user_config"],
        json!({ "theme": "light", "label": "a" })
    );

    // Negative: the same body with no query changes nothing.
    let det = body_to_json(patch_config(&state, "test.reset", json!({})).await).await;
    assert_eq!(
        det["user_config"],
        json!({ "theme": "light", "label": "a" }),
        "an empty Save must not be a reset"
    );

    // Positive: with the query, the stored map is discarded and defaults rule.
    let det =
        body_to_json(patch_config_query(&state, "test.reset", "?reset=true", json!({})).await)
            .await;
    assert_eq!(det["user_config"], json!({}));
    assert_eq!(
        det["effective_config"]["theme"], "dark",
        "and the manifest default is in force again: {det:?}"
    );
}

/// **P1-1 (round 3).** The per-write cap bounds each request; it does not
/// bound the row. Residue (a key an older manifest declared and this one does
/// not) is excluded from that cap on purpose — cell 12 — and nothing else
/// removes it: the write stores the whole document, `reload` does not touch
/// `user_config`, and `effective_config` only ignores it on read. So
/// `declare {k} → fill k → narrow the schema → reload → fill the next key`
/// adds ~8 KiB per turn, every step a legal 200, and the row is re-serialized
/// into every plugin-detail response.
///
/// This drives that exact loop until the kernel refuses. The refusal has to
/// name a way out or it is the lockout cell 12 was written to avoid, so the
/// reverse leg is asserted immediately after: the *same* request with
/// `?reset=true` succeeds and keeps the key the operator sent.
#[tokio::test]
async fn residue_cannot_grow_the_stored_config_without_bound() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin_with_config(
        src_root.path(),
        "test.grow",
        json!({
            "type": "object",
            "properties": { "k0": { "type": "string" } },
            "additionalProperties": false
        }),
    );
    install(&state, &src_dir).await;
    let manifest_path = src_dir.join("manifest.json");

    // Comfortably inside the 8192-byte per-write cap, so every one of these
    // requests is legal on its own terms — which is the point.
    let chunk = "x".repeat(8000);
    let one = |key: &str, value: &str| {
        let mut m = serde_json::Map::new();
        m.insert(key.to_string(), json!(value));
        Value::Object(m)
    };
    let mut refusal: Option<(usize, String)> = None;

    for round in 0..8usize {
        let key = format!("k{round}");
        if round > 0 {
            // The schema narrows to the next key only: everything written so
            // far becomes residue.
            let mut m: Value =
                serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
            let mut props = serde_json::Map::new();
            props.insert(key.clone(), json!({ "type": "string" }));
            m["config_schema"] = json!({
                "type": "object",
                "properties": Value::Object(props),
                "additionalProperties": false
            });
            std::fs::write(&manifest_path, serde_json::to_string_pretty(&m).unwrap()).unwrap();
            let resp = post_json(
                app(state.clone()),
                "/api/plugins/test.grow/reload",
                json!({}),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::OK, "reload failed on {round}");
        }

        let resp = patch_config(&state, "test.grow", one(&key, &chunk)).await;
        if resp.status() == StatusCode::BAD_REQUEST {
            refusal = Some((round, body_to_text(resp).await));
            break;
        }
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "round {round} is a legal single write: {}",
            body_to_text(resp).await
        );
        // Each accepted write leaves the row bigger than the per-write cap
        // allows for a single document — evidence that the growth is real and
        // not an artifact of the loop.
        if round >= 1 {
            let det =
                body_to_json(get_path(app(state.clone()), "/api/plugins/test.grow").await).await;
            let bytes = det["user_config"].to_string().len();
            assert!(
                bytes > 8192,
                "round {round} should already be past the per-write cap, got {bytes}"
            );
        }
    }

    let (round, text) = refusal.expect("the row must stop growing at some point");
    assert!(round > 1, "the cap must not refuse an ordinary first write");
    assert!(
        text.contains("32768"),
        "the refusal names the total cap: {text}"
    );
    assert!(
        text.contains("reset=true"),
        "a refusal on bytes no ordinary patch can shrink must name the exit: {text}"
    );

    // The exit is real, and it is the *same* request: reset drops the residue
    // and keeps exactly what the operator sent. If the cap were a precondition
    // on the stored row instead of a check on the result, this would refuse
    // too, and the operator would be stuck with a row no API can shrink.
    let key = format!("k{round}");
    let resp = patch_config_query(&state, "test.grow", "?reset=true", one(&key, &chunk)).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "the named recovery action must actually recover: {}",
        body_to_text(resp).await
    );
    let det = body_to_json(resp).await;
    assert_eq!(
        det["user_config"],
        one(&key, &chunk),
        "reset kept the current configuration and dropped only the residue"
    );
}

/// **P1-2 (round 3).** The config write is a read-modify-write over
/// `user_config` and used to take no lifecycle guard at all, while `enable`,
/// `disable`, `reload` and `uninstall` all take one — so it could interleave
/// with itself (two PATCHes both read the old row; the loser's key is dropped
/// from the winner's write) and with a `reload` (judge against the schema the
/// registry holds now, store for the schema a consumer reads next —
/// `effective_config` type-checks nothing on read, so that value goes to S2/S3
/// verbatim).
///
/// The witness holds the *real* lock, which `try_lock_lifecycle` is `pub` for
/// (design §5 R7), and asserts the refusal is the existing 409 `plugin_busy`
/// rather than a new code — plus that it is inert, which is what makes a
/// bare retry the whole remedy.
#[tokio::test]
async fn a_config_write_refuses_while_another_lifecycle_operation_holds_the_plugin() {
    use axum::extract::FromRef;

    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin_with_config(src_root.path(), "test.busy", stub_config_schema());
    install(&state, &src_dir).await;
    let det =
        body_to_json(patch_config(&state, "test.busy", json!({ "label": "before" })).await).await;
    assert_eq!(det["user_config"], json!({ "label": "before" }));

    let cs = calm_server::state::CodexShellState::from_ref(&state);
    let guard = cs
        .plugin
        .try_lock_lifecycle("test.busy")
        .expect("lock free");

    let resp = patch_config(&state, "test.busy", json!({ "label": "during" })).await;
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "a config write may not interleave with another lifecycle operation"
    );
    let body = body_to_json(resp).await;
    assert_eq!(
        body["code"], "plugin_busy",
        "the §2.4 three-state table already owns this cell — no new code: {body}"
    );

    // Inert: the refusal wrote nothing, so retrying is the entire remedy.
    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/test.busy").await).await;
    assert_eq!(det["user_config"], json!({ "label": "before" }));

    drop(guard);
    let resp = patch_config(&state, "test.busy", json!({ "label": "during" })).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "…and the identical request succeeds once the holder is gone: {}",
        body_to_text(resp).await
    );
    assert_eq!(
        body_to_json(resp).await["user_config"],
        json!({ "label": "during" })
    );
}

/// The two body shapes that had no witness at all: a non-object request body,
/// and the empty patch. `{}` is the request a form makes when the operator
/// saves without changing anything, and it must be a 200 no-op — not an error,
/// and not a write of `{}` over the stored map.
#[tokio::test]
async fn patch_config_rejects_a_non_object_body_and_accepts_an_empty_one() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin_with_config(src_root.path(), "test.body", stub_config_schema());
    install(&state, &src_dir).await;
    let det =
        body_to_json(patch_config(&state, "test.body", json!({ "label": "keep" })).await).await;
    assert_eq!(det["user_config"], json!({ "label": "keep" }));

    for body in [json!(["not", "an", "object"]), json!("nope"), json!(7)] {
        let resp = patch_config(&state, "test.body", body.clone()).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "body {body}");
        let text = body_to_text(resp).await;
        assert!(text.contains("must be a JSON object"), "got: {text}");
    }

    let resp = patch_config(&state, "test.body", json!({})).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let det = body_to_json(resp).await;
    assert_eq!(
        det["user_config"],
        json!({ "label": "keep" }),
        "an empty patch changes nothing"
    );
}

/// The "no schema ⇒ always 400" cell that the empty body could have escaped:
/// with no schema there is nothing to validate, so a validator-shaped
/// implementation would have let `{}` through. The refusal is about the plugin,
/// not about the body.
#[tokio::test]
async fn patch_config_on_a_plugin_without_a_schema_is_400_even_for_an_empty_body() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    install(
        &state,
        &write_stub_plugin(src_root.path(), "test.noschema2"),
    )
    .await;

    for body in [json!({}), json!({ "theme": null })] {
        let resp = patch_config(&state, "test.noschema2", body.clone()).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "body {body}");
        assert!(body_to_text(resp).await.contains("config_schema"));
    }
}

/// §2.5 — `has_config` on the list row. Both cells, because a constant `true`
/// or a constant `false` would each satisfy one of them alone.
#[tokio::test]
async fn list_reports_has_config_per_plugin() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    install(
        &state,
        &write_stub_plugin_with_config(src_root.path(), "test.with", stub_config_schema()),
    )
    .await;
    install(&state, &write_stub_plugin(src_root.path(), "test.without")).await;

    let rows = body_to_json(get_path(app(state.clone()), "/api/plugins").await).await;
    let rows = rows.as_array().expect("list is an array");
    let find = |id: &str| {
        rows.iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("row {id} missing from {rows:?}"))
            .clone()
    };
    assert_eq!(find("test.with")["has_config"], json!(true));
    assert_eq!(find("test.without")["has_config"], json!(false));

    // **P0-A**, the other cell: `has_config` on the list and `config_schema`
    // on the detail are the same bit read twice, so they must agree — the list
    // promising a form the detail cannot supply is the shape round 1 shipped.
    let with = body_to_json(get_path(app(state.clone()), "/api/plugins/test.with").await).await;
    assert_eq!(with["config_schema"], stub_config_schema());
    let without =
        body_to_json(get_path(app(state.clone()), "/api/plugins/test.without").await).await;
    assert!(
        without.get("config_schema").is_none(),
        "no schema declared ⇒ none published: {without:?}"
    );
}

/// §2.3 — defaults are applied on READ and never stored, which is what keeps a
/// later manifest free to change a default for already-configured installs.
#[tokio::test]
async fn detail_carries_effective_config_without_persisting_defaults() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin_with_config(src_root.path(), "test.eff", stub_config_schema());
    install(&state, &src_dir).await;

    // Untouched install: the default is visible in effective, absent in stored.
    let det = body_to_json(get_path(app(state.clone()), "/api/plugins/test.eff").await).await;
    assert_eq!(det["user_config"], json!({}), "nothing persisted yet");
    assert_eq!(det["effective_config"], json!({ "theme": "dark" }));

    // A write of an unrelated key must not materialize the default either —
    // this is the cell that fails if defaults were merged before storing.
    let det = body_to_json(patch_config(&state, "test.eff", json!({ "retries": 2 })).await).await;
    assert_eq!(det["user_config"], json!({ "retries": 2 }));
    assert_eq!(
        det["effective_config"],
        json!({ "theme": "dark", "retries": 2 })
    );
}

// ===========================================================================
// #1196 S0b — the leading 404 probes and reload's `if plug.enabled` guard.
//
// These five tests assert endpoint contracts: an unknown id must 404 on all
// four lifecycle endpoints, and reload must not respawn a disabled plugin.
// They are not all the same kind of gate, so what each was observed to do is
// spelled out rather than assumed:
//
// * `reload`'s leading probe is the only one that is single-point observable.
//   Deleting it alone was observed to turn the 404 into a manifest-read 400
//   (`left: 400 / right: 404`).
// * `enable` / `disable` / `uninstall` are *not* single-point observable
//   today. `plugin_update_enabled` and `plugin_delete` both already return
//   `NotFound` on `rows_affected() == 0`, and deleting one of those three
//   probes alone was observed to leave the suite green. These three tests gate
//   the endpoint contract, not the probe line; only a compound mutation that
//   also removes the repo fallback turns them red.
// * `reload_disabled_plugin_does_not_spawn` was observed red under deletion of
//   reload's `if plug.enabled` respawn guard.
//
// Design §7 nail 7 (`uninstall`'s three `let _ =`) has **no** gate here or
// anywhere in the suite — see that method's doc comment.
//
// S1 rewrites every step of these methods, which is why the contracts are
// pinned even where today's probe line is redundant with the repo layer.
// ===========================================================================

const GHOST: &str = "test.no.such.plugin";

#[tokio::test]
async fn enable_unknown_id_returns_404() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let resp = post_json(
        app(state),
        &format!("/api/plugins/{GHOST}/enable"),
        json!({}),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "endpoint contract: enable of an unknown id must 404"
    );
}

#[tokio::test]
async fn disable_unknown_id_returns_404() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let resp = post_json(
        app(state),
        &format!("/api/plugins/{GHOST}/disable"),
        json!({}),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "endpoint contract: disable of an unknown id must 404"
    );
}

/// Endpoint contract only. `plugin_delete` itself returns `NotFound` on
/// `rows_affected() == 0` (`out_of_domain.rs:464-472`), so deleting
/// `uninstall`'s leading probe alone was observed to leave this test green;
/// the 204 shape needs a compound mutation that also drops that repo fallback.
#[tokio::test]
async fn uninstall_unknown_id_returns_404_not_204() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let resp = delete_path(app(state), &format!("/api/plugins/{GHOST}")).await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "uninstall of an unknown id must 404; a 204 would mean `plugin_delete` \
         silently absorbed the missing row"
    );
}

/// Without the leading probe, `reload` walks on to read
/// `<install_path>/manifest.json` off an empty `install_path` and reports the
/// io error as a 400 `plugin_install`.
#[tokio::test]
async fn reload_unknown_id_returns_404_not_manifest_read_error() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let resp = post_json(
        app(state),
        &format!("/api/plugins/{GHOST}/reload"),
        json!({}),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "reload of an unknown id must 404, not a manifest-read 400/500"
    );
}

/// Design §7 nail 6: reload respawns **only if the pre-stop row said
/// `enabled`**. An unconditional respawn resurrects a plugin the operator
/// disabled.
///
/// The assertions read the host's runtime table, not just the HTTP code:
/// `PluginHost::spawn` is awaited to completion inside `reload`, so if the
/// guard is gone the id has a live (or admission-reserved) entry by the time
/// the response is built.
///
/// What each assertion does, and what has actually been observed of it:
///
/// * the `status(id).is_none()` *before* the reload is a **precondition**, and
///   today a vacuous one — `install` has no runtime step at all, so it can
///   never fail. It is kept as documentation of the starting state, not as a
///   gate;
/// * `det["state"] == "disabled"` is rendered by `build_detail` from
///   `PluginHost::status(id)`. Under the mutation this test is named for
///   (deleting the `if plug.enabled` guard) *this* is the assertion observed
///   to go red, with `left: "running" / right: "disabled"`;
/// * the trailing `list_running().is_empty()` reads the whole live table via a
///   different host method. Under that same mutation it never executes,
///   because `det["state"]` fails first, and no mutation is known today under
///   which it alone goes red: `status(id)` already reports admission-reserved
///   ids before consulting the live map (`mod.rs:1819-1826`), and `reload`
///   spawns `id` itself, having already rejected `manifest.id != id`. It is
///   kept as redundant defence against future changes; no additional witness
///   for it has been observed.
#[tokio::test]
async fn reload_disabled_plugin_does_not_spawn() {
    let (state, _tmp, _plugins_dir) = boot_state().await;
    let src_root = tempfile::tempdir().unwrap();
    let src_dir = write_stub_plugin(src_root.path(), "test.reload.disabled");
    let resp = post_json(
        app(state.clone()),
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": src_dir.to_string_lossy() } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    // Install leaves the row disabled; never enable it.
    assert!(
        state.plugin.status("test.reload.disabled").await.is_none(),
        "freshly installed plugin must not be running"
    );

    let resp = post_json(
        app(state.clone()),
        "/api/plugins/test.reload.disabled/reload",
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "reload should 200");
    let det = body_to_json(resp).await;
    assert_eq!(det["enabled"], false, "reload must not flip `enabled`");
    assert_eq!(
        det["state"], "disabled",
        "a disabled plugin must still read `disabled` after reload"
    );

    // Reads the whole live table via `list_running`, a different host method
    // from the `status(id)` behind `det["state"]` above. Under this test's
    // named mutation (deleting the `if plug.enabled` guard in
    // `PluginHost::reload`) `det["state"]` goes red first ("running" vs
    // "disabled") and this line never runs; no mutation is known today under
    // which this line alone goes red. Kept as redundant defence against future
    // changes — see the doc comment.
    let running = state.plugin.list_running().await;
    assert!(
        running.is_empty(),
        "reload of a disabled plugin must not spawn anything (design §7 \
         nail 6); running: {:?}",
        running.iter().map(|s| &s.id).collect::<Vec<_>>()
    );
}

// The pre-M5 `iframe_write_without_cookie_returns_401` test exercised the
// `iframe-write` REST surface, which was deleted in M5 alongside the cookie
// cache (see migration doc §3.3). M5's replacement gate is the `neige.*`
// prefix check on `POST /api/plugins/:id/tool-call`, covered by
// `plugin_routes_m5.rs::tool_call_rejects_non_neige_namespace`.
