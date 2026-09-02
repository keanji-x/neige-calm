//! #1110 S6 — seed workflow template waves; auto-fork on create.
//!
//! Matching `template_id` lazily seeds three system-cove template waves
//! (overlay `template_key`) and forks that report when `fork_report_from`
//! is omitted. Lists still hide templates; detail returns them. Explicit
//! `fork_report_from` is not overwritten.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{EditAuthor, EventBus};
use calm_server::ids::ActorId;
use calm_server::model::{NewCove, NewPlugin};
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, DaemonClient};
use calm_server::wave_cove_cache::WaveCoveCache;
use calm_server::wave_report::{WaveReportPayload, persist_report, resolve_report_for_wave};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::common;
use crate::support::git_helpers::attached_repo_fixture;

const ISSUE_DEVELOPMENT: &str = "issue-development";
const SMALL_CHANGE: &str = "small-change";
const INVESTIGATION: &str = "investigation";
const TEMPLATE_KEYS: [&str; 3] = [ISSUE_DEVELOPMENT, SMALL_CHANGE, INVESTIGATION];

struct Boot {
    app: axum::Router,
    state: AppState,
    cove_id: String,
    repo: Arc<dyn Repo>,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "workflow-template-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient {
            data_dir: tmp.path().to_path_buf(),
            proc_supervisor_sock: None,
        }),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-1110-s6"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone()),
        )),
        Arc::new(common::fake_codex_client()),
        Some(card_role_cache),
        Some(wave_cove_cache),
    );
    let shared = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let state = state.with_shared_codex_appserver(shared);
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());
    Boot {
        app,
        state,
        cove_id: cove.id.to_string(),
        repo,
        _tmp: tmp,
    }
}

fn theme() -> Value {
    json!({"fg": [216, 219, 226], "bg": [15, 20, 24]})
}

async fn post(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// Like [`post`], but returns the body as raw text.
///
/// Extractor-level rejections (`#1209` test #16) are produced by axum, not by
/// this crate's `CalmError`, so they are `text/plain` and not the usual
/// `{"error": ...}` envelope. Parsing them as JSON yields `null` and throws the
/// message away, which would leave the assertions unable to tell an
/// unknown-field rejection from any other 4xx.
async fn post_text(app: axum::Router, uri: &str, body: Value) -> (StatusCode, String) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn get(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn spec_harness_ops_for_wave(repo: &Arc<dyn Repo>, wave_id: &str) -> i64 {
    let pool = repo.sqlite_pool().expect("sqlite pool");
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM operations \
         WHERE kind = 'spec-harness-start' \
           AND json_extract(payload_json, '$.wave_id') = ?1",
    )
    .bind(wave_id)
    .fetch_one(&pool)
    .await
    .expect("wave spec-harness-start count")
}

fn create_body(cove_id: &str, title: &str, extra: Value) -> Value {
    let mut body = json!({
        "cove_id": cove_id,
        "title": title,
        "cwd": attached_repo_fixture(&format!("1110-s6-{title}")),
        "attach_folder": true,
        "theme": theme(),
    });
    if let Value::Object(extra) = extra
        && let Value::Object(obj) = &mut body
    {
        obj.extend(extra);
    }
    body
}

/// #1209 test #10/#12/#13 — a whole-database snapshot.
///
/// Every user table, every column, every row, rendered through SQLite's own
/// `quote()` so NULL, text and integer stay distinguishable, ordered so the
/// digest is stable. Deliberately **not** a hand-maintained list of tables or
/// of overlay entity kinds: the failure mode these tests exist to catch is "a
/// read quietly wrote something", and a snapshot that enumerates what it looks
/// at silently stops covering whatever is added next. `seeded_templates` below
/// is the opposite shape — it only sees `kind == "template"` overlays — which
/// is why these tests do not reuse it.
///
/// (The #1209 design predicted this had to be assembled from `Repo` trait
/// accessors because tests "cannot write raw SQL". Not so in this file:
/// `Repo::sqlite_pool` is public and `spec_harness_ops_for_wave` above already
/// uses it.)
async fn db_snapshot(repo: &Arc<dyn Repo>) -> Vec<(String, String)> {
    let pool = repo.sqlite_pool().expect("sqlite pool");
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master \
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name <> '_sqlx_migrations' \
         ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .expect("table list");
    assert!(!tables.is_empty(), "snapshot found no tables to compare");
    let mut snapshot = Vec::with_capacity(tables.len());
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
        let digest: String = sqlx::query_scalar(&format!(
            "SELECT coalesce(group_concat(row_text, char(10)), '') FROM \
             (SELECT {row_text} AS row_text FROM \"{table}\" ORDER BY 1)"
        ))
        .fetch_one(&pool)
        .await
        .unwrap_or_else(|error| panic!("digest of {table}: {error}"));
        snapshot.push((table, digest));
    }
    snapshot
}

async fn seeded_templates(repo: &Arc<dyn Repo>) -> Vec<(String, String)> {
    let overlays = repo
        .overlays_by_kind("view")
        .await
        .expect("template overlays");
    let mut keyed = Vec::new();
    for overlay in overlays {
        if overlay.plugin_id != "kernel" || overlay.kind != "template" {
            continue;
        }
        let Some(key) = overlay.payload.get("template_key").and_then(Value::as_str) else {
            continue;
        };
        keyed.push((key.to_string(), overlay.entity_id));
    }
    keyed.sort_by(|left, right| left.0.cmp(&right.0));
    keyed
}

fn report_card_payload(detail: &Value) -> WaveReportPayload {
    let card = detail["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["kind"] == "wave-report")
        .expect("wave-report card");
    serde_json::from_value(card["payload"].clone()).expect("report payload")
}

fn task_blocks(payload: &WaveReportPayload) -> Vec<&Value> {
    payload
        .blocks
        .as_ref()
        .into_iter()
        .flatten()
        .filter(|block| block.kind == "task")
        .map(|block| &block.payload)
        .collect()
}

#[tokio::test]
async fn matching_template_id_seeds_one_wave_per_template_key() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        create_body(
            &boot.cove_id,
            "first-issue-dev",
            json!({ "template_id": ISSUE_DEVELOPMENT }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    let first = seeded_templates(&boot.repo).await;
    let first_keys: Vec<&str> = first.iter().map(|(key, _)| key.as_str()).collect();
    let mut expected = TEMPLATE_KEYS.to_vec();
    expected.sort_unstable();
    assert_eq!(first_keys, expected, "seeded keys={first:?}");
    assert_eq!(first.len(), TEMPLATE_KEYS.len());
    for (key, wave_id) in &first {
        let (status, detail) = get(boot.app.clone(), &format!("/api/waves/{wave_id}")).await;
        assert_eq!(status, StatusCode::OK, "key={key} body={detail}");
        assert_eq!(detail["wave"]["id"], *wave_id);
        assert_eq!(
            spec_harness_ops_for_wave(&boot.repo, wave_id).await,
            0,
            "template `{key}` must skip spec-harness-start"
        );
    }

    let (status, listed) = get(
        boot.app.clone(),
        &format!("/api/coves/{}/waves", boot.cove_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={listed}");
    let user_ids: Vec<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|wave| wave["id"].as_str().unwrap())
        .collect();
    assert!(user_ids.contains(&body["id"].as_str().unwrap()));
    for (_, wave_id) in &first {
        assert!(
            !user_ids.contains(&wave_id.as_str()),
            "template {wave_id} leaked into user cove list; ids={user_ids:?}"
        );
    }

    let system_cove_id = boot
        .repo
        .cove_get_system()
        .await
        .unwrap()
        .expect("system cove")
        .id
        .to_string();
    let (status, system_list) = get(
        boot.app.clone(),
        &format!("/api/coves/{system_cove_id}/waves"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={system_list}");
    let system_ids: Vec<&str> = system_list
        .as_array()
        .unwrap()
        .iter()
        .map(|wave| wave["id"].as_str().unwrap())
        .collect();
    for (_, wave_id) in &first {
        assert!(
            !system_ids.contains(&wave_id.as_str()),
            "template {wave_id} leaked into system cove list; ids={system_ids:?}"
        );
    }

    let (status, global) = get(boot.app.clone(), "/api/waves").await;
    assert_eq!(status, StatusCode::OK, "body={global}");
    let global_ids: Vec<&str> = global
        .as_array()
        .unwrap()
        .iter()
        .map(|wave| wave["id"].as_str().unwrap())
        .collect();
    for (_, wave_id) in &first {
        assert!(
            !global_ids.contains(&wave_id.as_str()),
            "template {wave_id} leaked into GET /api/waves; ids={global_ids:?}"
        );
    }

    let (status, _) = post(
        boot.app.clone(),
        "/api/waves",
        create_body(
            &boot.cove_id,
            "second-issue-dev",
            json!({ "template_id": SMALL_CHANGE }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let second = seeded_templates(&boot.repo).await;
    assert_eq!(first, second, "second matching create must not duplicate");
}

#[tokio::test]
async fn issue_development_create_forks_inspect_issue_not_ready() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        create_body(
            &boot.cove_id,
            "forked-issue-dev",
            json!({ "template_id": ISSUE_DEVELOPMENT }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    assert_eq!(body["template_id"], ISSUE_DEVELOPMENT);
    assert!(
        body["plugin_scope"].is_null(),
        "empty plugin registry leaves plugin_scope null, body={body}"
    );
    let wave_id = body["id"].as_str().expect("wave id");
    assert!(
        spec_harness_ops_for_wave(&boot.repo, wave_id).await >= 1,
        "forked user wave still starts spec harness"
    );

    let (status, detail) = get(boot.app.clone(), &format!("/api/waves/{wave_id}")).await;
    assert_eq!(status, StatusCode::OK, "detail={detail}");
    let payload = report_card_payload(&detail);
    assert!(
        payload.report_startup_read_required(),
        "forked plan must require a startup read"
    );
    let tasks = task_blocks(&payload);
    let inspect = tasks
        .iter()
        .find(|task| task["key"] == "inspect-issue")
        .unwrap_or_else(|| panic!("missing inspect-issue; tasks={tasks:?}"));
    assert_eq!(inspect["ready"], false);
    assert_eq!(inspect["kind"], "codex");
    assert_eq!(inspect["declared_by"], "spec");
    assert!(
        inspect["context"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool == "gh.issue.view"),
        "inspect-issue must keep context.tools; payload={inspect}"
    );
    let implement = tasks
        .iter()
        .find(|task| task["key"] == "implement-change")
        .expect("implement-change");
    assert!(
        implement.get("gate").is_none(),
        "implement-change must not carry an executed gate; payload={implement}"
    );
    assert!(
        implement["no_gate_reason"]
            .as_str()
            .unwrap_or("")
            .contains("author a real gate"),
        "implement-change must tell spec to author a real gate; payload={implement}"
    );
}

#[tokio::test]
async fn explicit_fork_report_from_is_not_overwritten() {
    let boot = boot().await;
    let (status, source) = post(
        boot.app.clone(),
        "/api/waves",
        create_body(&boot.cove_id, "custom-source", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={source}");
    let source_id = source["id"].as_str().unwrap().to_string();
    let (source_wave, report_card, current) =
        resolve_report_for_wave(boot.repo.as_ref(), &source_id)
            .await
            .expect("source report");
    let if_doc_rev = current.doc_rev;
    persist_report(
        boot.repo.as_ref(),
        &boot.state.events,
        boot.state.write(),
        ActorId::User,
        EditAuthor::User,
        source_wave,
        report_card,
        current,
        WaveReportPayload::new(
            "custom source summary",
            "# Custom\n\nnot-the-issue-development-plan\n",
        ),
        if_doc_rev,
        None,
        None,
        false,
    )
    .await
    .expect("stamp custom source report");

    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        create_body(
            &boot.cove_id,
            "explicit-fork",
            json!({
                "template_id": ISSUE_DEVELOPMENT,
                "fork_report_from": source_id,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let wave_id = body["id"].as_str().expect("wave id");
    let (status, detail) = get(boot.app.clone(), &format!("/api/waves/{wave_id}")).await;
    assert_eq!(status, StatusCode::OK, "detail={detail}");
    let payload = report_card_payload(&detail);
    assert!(
        payload.body.contains("not-the-issue-development-plan"),
        "explicit fork_report_from must win; body={}",
        payload.body
    );
    assert!(
        !payload.body.contains("inspect-issue"),
        "issue-development plan must not replace an explicit fork; body={}",
        payload.body
    );
}

#[tokio::test]
async fn investigation_and_small_change_auto_fork_without_plugin() {
    let boot = boot().await;
    for (key, task_key) in [(SMALL_CHANGE, "inspect"), (INVESTIGATION, "gather-facts")] {
        let (status, body) = post(
            boot.app.clone(),
            "/api/waves",
            create_body(
                &boot.cove_id,
                &format!("forked-{key}"),
                json!({ "template_id": key }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "key={key} body={body}");
        assert_eq!(body["template_id"], key);
        assert!(body["plugin_scope"].is_null());
        let wave_id = body["id"].as_str().unwrap();
        let (status, detail) = get(boot.app.clone(), &format!("/api/waves/{wave_id}")).await;
        assert_eq!(status, StatusCode::OK);
        let payload = report_card_payload(&detail);
        assert!(payload.report_startup_read_required());
        let tasks = task_blocks(&payload);
        let first = tasks
            .iter()
            .find(|task| task["key"] == task_key)
            .unwrap_or_else(|| panic!("missing {task_key} for {key}; tasks={tasks:?}"));
        assert_eq!(first["ready"], false);
    }
}

#[tokio::test]
async fn stolen_user_cove_template_key_does_not_hijack_auto_fork() {
    let boot = boot().await;
    let (status, stolen) = post(
        boot.app.clone(),
        "/api/waves",
        create_body(
            &boot.cove_id,
            "stolen-template",
            json!({ "as_template": true }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={stolen}");
    let stolen_id = stolen["id"].as_str().unwrap().to_string();
    let (status, overlay) = post(
        boot.app.clone(),
        "/api/overlays",
        json!({
            "plugin_id": "kernel",
            "entity_kind": "view",
            "entity_id": stolen_id,
            "kind": "template",
            "payload": { "schemaVersion": 1, "template_key": ISSUE_DEVELOPMENT }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={overlay}");
    let (stolen_wave, report_card, current) =
        resolve_report_for_wave(boot.repo.as_ref(), &stolen_id)
            .await
            .expect("stolen report");
    let if_doc_rev = current.doc_rev;
    persist_report(
        boot.repo.as_ref(),
        &boot.state.events,
        boot.state.write(),
        ActorId::User,
        EditAuthor::User,
        stolen_wave,
        report_card,
        current,
        WaveReportPayload::new(
            "stolen user-cove template",
            "# Stolen\n\nstolen-user-cove-plan\n",
        ),
        if_doc_rev,
        None,
        None,
        false,
    )
    .await
    .expect("stamp stolen report");

    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        create_body(
            &boot.cove_id,
            "after-stolen-key",
            json!({ "template_id": ISSUE_DEVELOPMENT }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let wave_id = body["id"].as_str().expect("wave id");
    let (status, detail) = get(boot.app.clone(), &format!("/api/waves/{wave_id}")).await;
    assert_eq!(status, StatusCode::OK, "detail={detail}");
    let payload = report_card_payload(&detail);
    assert!(
        payload.body.contains("inspect-issue"),
        "auto-fork must still use the kernel plan; body={}",
        payload.body
    );
    assert!(
        !payload.body.contains("stolen-user-cove-plan"),
        "user-cove stolen template_key must not hijack auto-fork; body={}",
        payload.body
    );
    let templates = seeded_templates(&boot.repo).await;
    assert!(
        templates
            .iter()
            .any(|(key, id)| key == ISSUE_DEVELOPMENT && id != &stolen_id),
        "kernel seed must still mint a system-cove issue-development; templates={templates:?}"
    );
}

#[tokio::test]
async fn unknown_template_id_still_400s() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app,
        "/api/waves",
        create_body(
            &boot.cove_id,
            "unknown-workflow",
            json!({ "template_id": "missing-workflow" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    // #1209 — three legs, because the interesting regression is not "some 400
    // happened" but "the 400 was decided by the roster". Leg 2 is the one that
    // catches restoring the registry wording (and with it the registry as the
    // admission authority); a `.contains("missing-workflow")`-only assertion
    // was green both before and after that change.
    let error = body["error"].as_str().unwrap_or("");
    assert!(error.contains("known wave template"), "body={body}");
    assert!(
        !error.contains("registered trusted workflow"),
        "body={body}"
    );
    assert!(error.contains("missing-workflow"), "body={body}");
}

/// #1209 test #8's fixture — a **running, trusted** plugin whose manifest
/// declares the ids passed in.
///
/// Two things about it are load-bearing and must not be "tidied" into the
/// shared `boot()`:
///
/// 1. **No `input_schema`.** Copying the stub in
///    `tests/cases/wave_templates_read.rs` verbatim brings one along, and its
///    `required` list makes a create *without* `template_input` fail the
///    required-input check (`validate_template_input_binding`) — a 400 that
///    arrives no matter what the admission rule is. Test #8 would then be green
///    even with the pre-#1209 plugin fallback restored, i.e. green precisely
///    when the thing it exists to detect is back.
/// 2. **It is a separate boot.** `boot()` starts no plugins, and
///    `create_accepts_exactly_the_listed_templates` depends on that (see its
///    doc comment). Merging the two fixtures would break that test for reasons
///    that have nothing to do with admission.
async fn boot_with_trusted_plugin(declared_template_ids: &[&str]) -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "workflow-template-plugin-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();

    // Mirrors `forge_trust::trusted_forge_plugin`'s default so the stub is
    // trusted without mutating process env.
    let plugin_id = std::env::var("NEIGE_TRUSTED_FORGE_PLUGINS")
        .ok()
        .and_then(|configured| {
            configured
                .split(',')
                .map(str::trim)
                .find(|id| !id.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "dev.neige.git-forge".to_string());
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let install_dir = plugins_dir.join(&plugin_id);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("create plugin bin dir");
    std::fs::create_dir_all(&plugins_data_dir).expect("create plugin data dir");
    std::os::unix::fs::symlink(
        std::path::Path::new(env!("CARGO_BIN_EXE_plugin-host-stub-echo")),
        bin_dir.join("stub"),
    )
    .expect("symlink stub plugin");

    let workflows: Vec<Value> = declared_template_ids
        .iter()
        .map(|id| json!({ "id": id }))
        .collect();
    let manifest = Manifest::parse(
        &json!({
            "manifest_version": 1,
            "id": plugin_id,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Trusted workflow owner",
            "entrypoint": { "command": "bin/stub" },
            // No `input_schema`: see this function's doc comment, point 1.
            "workflows": workflows,
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
    plugin_host.spawn(&plugin_id).await.expect("spawn plugin");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(status) = plugin_host.status(&plugin_id).await
            && matches!(status.status, PluginRuntimeStatus::Running)
        {
            break;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "plugin {plugin_id} did not reach Running within 5s"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient {
            data_dir: tmp.path().to_path_buf(),
            proc_supervisor_sock: None,
        }),
        plugin_host,
        Arc::new(common::fake_codex_client()),
        Some(card_role_cache),
        Some(wave_cove_cache),
    );
    let shared = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let state = state.with_shared_codex_appserver(shared);
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());
    Boot {
        app,
        state,
        cove_id: cove.id.to_string(),
        repo,
        _tmp: tmp,
    }
}

/// #1209 test #8 — **the test the whole slice rests on.**
///
/// A running, trusted plugin declares a workflow id that is not in the kernel's
/// template roster. Before #1209 that made the id creatable (201, with
/// `plugin_scope` stamped and nothing to fork); the create path asked the
/// plugin registry first and only consulted the roster as a fallback. #1209
/// inverts that: the roster is the admission test and the binding is an
/// attribute, so this create is a 400 — and the plugin's running/trusted state
/// cannot change that answer.
///
/// The mutation this must catch is restoring the fallback (an
/// `.or_else(|| resolve_trusted_workflow(..))` inside `admit_template`, in any
/// spelling). It then goes red on the **status code**, not on wording.
/// Restoring only the old wording turns leg 2 of the error assertion red.
#[tokio::test]
async fn plugin_declared_non_template_id_is_rejected() {
    const NOT_A_TEMPLATE: &str = "not-a-template";
    let boot = boot_with_trusted_plugin(&[NOT_A_TEMPLATE, ISSUE_DEVELOPMENT]).await;

    // Liveness control first: without it a broken fixture (plugin not running,
    // not trusted, manifest not registered) would make the real assertion below
    // pass for the wrong reason — an unbound id is rejected by *any* rule. This
    // create proves the plugin really does bind, on this very app, right now.
    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        create_body(
            &boot.cove_id,
            "bound-control",
            json!({ "template_id": ISSUE_DEVELOPMENT }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    assert!(
        !body["plugin_scope"].is_null(),
        "fixture is not actually binding — the real assertion below would be \
         vacuous; body={body}"
    );

    let before = db_snapshot(&boot.repo).await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        create_body(
            &boot.cove_id,
            "plugin-declared-non-template",
            json!({ "template_id": NOT_A_TEMPLATE }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a running trusted plugin must not make a non-roster id creatable; body={body}"
    );
    let error = body["error"].as_str().unwrap_or("");
    assert!(error.contains("known wave template"), "body={body}");
    assert!(
        !error.contains("requires `template_input`"),
        "rejected for input validation, not admission — the fixture's stub must \
         not declare an input_schema; body={body}"
    );
    assert!(
        !error.contains("registered trusted workflow"),
        "body={body}"
    );
    assert!(error.contains(NOT_A_TEMPLATE), "body={body}");
    // Cheap insurance, not a discriminating leg: a non-roster id does not seed
    // under the correct code *or* under the named mutation. It does catch a
    // fallback smuggled into the seeding branch (seed, then 500 on lookup).
    assert_eq!(
        db_snapshot(&boot.repo).await,
        before,
        "an admission 400 must not write anything"
    );
}

/// #1209 test #9 — the picker's list and create's accept set are one set.
///
/// **Premise, and it is load-bearing:** `boot()` starts no plugins. With no
/// running trusted plugin, `resolve_trusted_workflow` is `None` for every id,
/// so `validate_template_input_binding(None, None)` short-circuits `Ok(())` and
/// `issue-development` never reaches the required-input arm. That is what makes
/// `== 201` correct for *every* listed id. If this harness ever grows a plugin
/// fixture, this case must keep using the no-plugin one; if the premise has to
/// be relaxed, widen the assertion to an explicit allowance
/// (`201 || (400 && "requires `template_input`")`) — do **not** weaken it to
/// "the body does not say `known wave template`". That weaker form is green for
/// a re-worded special case such as
/// `if id == "investigation" { return Err(BadRequest("investigation is disabled")) }`,
/// which is exactly the disguise this test exists to strip off.
///
/// The forward direction is universally quantified over the listed ids. The
/// reverse ("create accepts nothing the list omits") is sampled here and
/// carried structurally by there being a single fallible roster lookup
/// (`workflow_template`), plus test #8 for the one concrete shape that
/// historically reintroduced a second path. It is not a set-equality gate and
/// is not claimed to be one.
#[tokio::test]
async fn create_accepts_exactly_the_listed_templates() {
    let boot = boot().await;
    let (status, listed) = get(boot.app.clone(), "/api/wave-templates").await;
    assert_eq!(status, StatusCode::OK, "body={listed}");
    let ids: Vec<String> = listed
        .as_array()
        .expect("array body")
        .iter()
        .map(|entry| entry["id"].as_str().expect("id string").to_string())
        .collect();
    // An empty list would make the loop below vacuously true.
    assert!(!ids.is_empty(), "the picker listed nothing: {listed}");

    for id in &ids {
        let (status, body) = post(
            boot.app.clone(),
            "/api/waves",
            create_body(
                &boot.cove_id,
                &format!("listed-{id}"),
                json!({ "template_id": id }),
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "listed template `{id}` was not creatable: {body}"
        );
    }

    for absent in ["definitely-not-a-template", "issue-development-x"] {
        let (status, body) = post(
            boot.app.clone(),
            "/api/waves",
            create_body(
                &boot.cove_id,
                &format!("absent-{absent}"),
                json!({ "template_id": absent }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "`{absent}`: body={body}");
        assert!(
            body["error"]
                .as_str()
                .unwrap_or("")
                .contains("known wave template"),
            "`{absent}`: body={body}"
        );
    }
}

/// #1209 test #10 (INV-1209-SEED) — reading the template list must not
/// materialize seed state.
///
/// Seeding is asymmetric on purpose: a *write* that names a template seeds the
/// three template waves; a *read* never does. Opening the New wave dialog would
/// otherwise mint a system cove, three waves and three reports, irreversibly,
/// and make "this database has never used a template" unobservable.
///
/// Both start states matter. Against an unseeded database only, a change that
/// writes solely on the already-seeded branch stays green; against a seeded one
/// only, a change that seeds stays green.
///
/// Only `GET /api/wave-templates` is exercised because it is the only
/// wave-template read route on this branch — `GET /api/wave-templates/{id}`
/// arrives with #1230 and this case must grow a leg for it at the merge.
///
/// Mutations that must turn this red: calling `ensure_workflow_templates` (or
/// `ensure_system_cove`) from the GET; rewriting a seeded template's report
/// summary in the GET; minting a wave with no overlay; writing only when
/// already seeded; appending a single `log_pure_event` pure event. The last
/// three are green against a `kind == "template"` overlay count, which is why
/// this uses a whole-database digest instead.
#[tokio::test]
async fn listing_wave_templates_does_not_materialize_seed_state() {
    // State A: nothing seeded.
    let boot = boot().await;
    assert!(
        seeded_templates(&boot.repo).await.is_empty(),
        "state A must start unseeded"
    );
    let before = db_snapshot(&boot.repo).await;
    let (status, body) = get(boot.app.clone(), "/api/wave-templates").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(
        db_snapshot(&boot.repo).await,
        before,
        "listing templates wrote to an unseeded database"
    );

    // State B: seeded, with readable reports.
    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        create_body(
            &boot.cove_id,
            "seed-for-inv-1209",
            json!({ "template_id": SMALL_CHANGE }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    assert_eq!(
        seeded_templates(&boot.repo).await.len(),
        TEMPLATE_KEYS.len(),
        "state B must start seeded"
    );
    let before = db_snapshot(&boot.repo).await;
    let (status, body) = get(boot.app.clone(), "/api/wave-templates").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(
        db_snapshot(&boot.repo).await,
        before,
        "listing templates wrote to an already-seeded database"
    );
}

/// #1209 test #12 — a whitespace-only `template_id` is rejected **by
/// admission**.
///
/// #1209 deleted the dedicated `trim().is_empty()` guard: whitespace is simply
/// not in the roster, so it takes the same path, the same status and the same
/// message as any other unknown id. Nothing in the Rust suite covered this
/// before, so deleting the guard would otherwise have deleted unpinned code.
///
/// The mutation this catches is the guard coming back as a *skip* —
/// `if id.trim().is_empty() { /* treat as no template chosen */ }`, yielding
/// 201, a null `plugin_scope` and no fork. `unknown_template_id_still_400s`
/// sends `missing-workflow` and stays green through that change; this one does
/// not. The request deliberately carries a valid `cove_id`, no `cwd` and no
/// `template_input`, so no other validation can supply the 400.
#[tokio::test]
async fn blank_template_id_is_rejected() {
    let boot = boot().await;
    let before = db_snapshot(&boot.repo).await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({
            "cove_id": boot.cove_id,
            "title": "blank workflow id",
            "theme": theme(),
            "template_id": "   ",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    let error = body["error"].as_str().unwrap_or("");
    assert!(error.contains("known wave template"), "body={body}");
    assert!(error.contains("got `   `"), "body={body}");
    assert_eq!(
        db_snapshot(&boot.repo).await,
        before,
        "a rejected create must not seed"
    );
}

/// #1209 test #13 — a pre-transaction 4xx leaves no seed behind.
///
/// Template seeding used to run first, so `POST /api/waves` with a good
/// template id and a bad `cove_id` minted a system cove, three template waves
/// and three reports and *then* returned 404. #1209 moves the seed after every
/// check this handler can make before opening the transaction. All three of
/// those checks are covered here; the mutation of moving the seed block back to
/// its old position turns all three red.
///
/// Explicitly **not** covered, and not an oversight: the in-transaction 400s
/// (an explicit `fork_report_from` that is missing or cross-cove), the
/// folder-claim 409, and post-commit materialize failures. Those are decided
/// after the seed on purpose — the authoritative check has to live inside the
/// transaction — and asserting "no side effect" for them would pin a promise
/// the code does not make.
/// Split into one test per leg on purpose: a single loop short-circuits on the
/// first failing leg, so a mutation that breaks all three would only ever be
/// observed on one of them and the other two would never have been shown to
/// discriminate.
async fn assert_pre_transaction_4xx_does_not_seed(
    name: &str,
    body_json: Value,
    expected: StatusCode,
) {
    let boot = boot().await;
    let mut body_json = body_json;
    if body_json["cove_id"] == json!("") {
        body_json["cove_id"] = json!(boot.cove_id);
    }
    let before = db_snapshot(&boot.repo).await;
    let (status, body) = post(boot.app.clone(), "/api/waves", body_json).await;
    assert_eq!(status, expected, "{name}: body={body}");
    assert!(
        seeded_templates(&boot.repo).await.is_empty(),
        "{name}: a pre-transaction 4xx seeded template waves"
    );
    assert_eq!(
        db_snapshot(&boot.repo).await,
        before,
        "{name}: a pre-transaction 4xx wrote to the database"
    );
}

#[tokio::test]
async fn pre_transaction_404_unknown_cove_with_template_does_not_seed() {
    assert_pre_transaction_4xx_does_not_seed(
        "cove 404",
        json!({
            "cove_id": "cove_does_not_exist",
            "title": "unknown cove",
            "theme": theme(),
            "template_id": SMALL_CHANGE,
        }),
        StatusCode::NOT_FOUND,
    )
    .await;
}

#[tokio::test]
async fn pre_transaction_400_relative_cwd_with_template_does_not_seed() {
    assert_pre_transaction_4xx_does_not_seed(
        "relative cwd",
        json!({
            "cove_id": "",
            "title": "relative cwd",
            "cwd": "relative/not/absolute",
            "attach_folder": false,
            "theme": theme(),
            "template_id": SMALL_CHANGE,
        }),
        StatusCode::BAD_REQUEST,
    )
    .await;
}

#[tokio::test]
async fn pre_transaction_400_non_repo_cwd_with_template_does_not_seed() {
    // An absolute, existing directory that is not a git repository.
    // `attach_folder` is deliberately left false: the guard keys off whether
    // `cwd` was supplied at all, not off `attach_folder`, and setting the
    // latter would suggest the check lives on that field.
    let non_repo = TempDir::new().expect("non-repo tempdir");
    assert_pre_transaction_4xx_does_not_seed(
        "cwd is not a git repository",
        json!({
            "cove_id": "",
            "title": "cwd not a repo",
            "cwd": non_repo.path().display().to_string(),
            "attach_folder": false,
            "theme": theme(),
            "template_id": SMALL_CHANGE,
        }),
        StatusCode::BAD_REQUEST,
    )
    .await;
}

// ---------------------------------------------------------------------------
// #1230 — editable templates, via a diff write endpoint.
//
// The write side takes `{title, edits:[{key,goal}], appends:[{key,goal}]}` and
// never a task list. Review round 2 found that accepting blocks let a client
// erase a tombstone by omission and store `released_by_user` / `spawn`
// verbatim; the diff shape makes both unexpressible rather than rejected, which
// is what these tests are here to hold.
// ---------------------------------------------------------------------------

async fn put(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(uri)
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "user")
                .body(Body::from(body.to_string()))
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

fn listed_template<'a>(body: &'a Value, id: &str) -> &'a Value {
    body.as_array()
        .expect("array body")
        .iter()
        .find(|entry| entry["id"] == id)
        .unwrap_or_else(|| panic!("template `{id}` missing from {body}"))
}

fn task_keys(template: &Value) -> Vec<&str> {
    template["tasks"]
        .as_array()
        .expect("tasks array")
        .iter()
        .map(|task| task["key"].as_str().expect("key"))
        .collect()
}

/// Acceptance 2 — a read of the picker must not mint the template waves.
///
/// The count assertion is the whole test: returning the right constants proves
/// nothing on its own, because the seeding path would also return the right
/// values. What must hold is that the read left the database as it found it.
#[tokio::test]
async fn listing_templates_returns_constants_without_seeding_anything() {
    let boot = boot().await;
    assert!(
        seeded_templates(&boot.repo).await.is_empty(),
        "precondition: nothing seeded before the read"
    );

    let (status, body) = get(boot.app.clone(), "/api/wave-templates").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(
        listed_template(&body, SMALL_CHANGE)["title"],
        "Small change"
    );
    assert_eq!(
        task_keys(listed_template(&body, SMALL_CHANGE))[0],
        "inspect"
    );

    assert!(
        seeded_templates(&boot.repo).await.is_empty(),
        "a GET must never trigger the lazy seed"
    );
}

/// Acceptance 1 — edit a goal, then check *both* readers: the picker and the
/// wave the create path actually produces. Asserting only one would pass while
/// the other drifted, which is the pre-#1230 state.
#[tokio::test]
async fn an_edited_goal_reaches_the_picker_and_the_forked_wave() {
    let boot = boot().await;
    const NEW_GOAL: &str = "Read the request and write down what it touches, then stop.";
    const NEW_TITLE: &str = "Tiny change";

    let (status, saved) = put(
        boot.app.clone(),
        &format!("/api/wave-templates/{SMALL_CHANGE}"),
        json!({ "title": NEW_TITLE, "edits": [{ "key": "inspect", "goal": NEW_GOAL }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={saved}");
    assert_eq!(saved["title"], NEW_TITLE);

    // Reader 1: the picker.
    let (status, body) = get(boot.app.clone(), "/api/wave-templates").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let listed = listed_template(&body, SMALL_CHANGE);
    assert_eq!(listed["title"], NEW_TITLE);
    assert_eq!(listed["tasks"][0]["goal"], NEW_GOAL);
    assert_eq!(
        listed_template(&body, INVESTIGATION)["title"],
        "Investigation"
    );

    // Reader 2: the wave the create path produces.
    let (status, created) = post(
        boot.app.clone(),
        "/api/waves",
        create_body(
            &boot.cove_id,
            "forked-after-edit",
            json!({ "template_id": SMALL_CHANGE }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={created}");
    let wave_id = created["id"].as_str().expect("wave id");
    let (status, detail) = get(boot.app.clone(), &format!("/api/waves/{wave_id}")).await;
    assert_eq!(status, StatusCode::OK, "body={detail}");
    let payload = report_card_payload(&detail);
    let goals: Vec<&str> = task_blocks(&payload)
        .iter()
        .map(|block| block["goal"].as_str().expect("goal string"))
        .collect();
    assert!(
        goals.contains(&NEW_GOAL),
        "the forked wave must carry the edited goal, got {goals:?}"
    );
}

/// A goal edit must leave every other field of that task exactly as it was.
///
/// The editor states two facts about a task; the other eight fields of the
/// block are the server's, and a save that flattened them would produce a
/// template whose forked waves have no acceptance criteria and no dependency
/// graph.
#[tokio::test]
async fn editing_a_goal_leaves_every_other_field_untouched() {
    let boot = boot().await;
    let uri = format!("/api/wave-templates/{ISSUE_DEVELOPMENT}");
    // Seed by saving a no-op-shaped edit, then read the stored blocks directly.
    let (status, body) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Issue development", "edits": [] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let before = template_task_blocks(&boot, ISSUE_DEVELOPMENT).await;
    assert!(
        before
            .iter()
            .any(|task| task.get("acceptance").is_some() && task.get("context").is_some()),
        "fixture must actually carry the fields we claim to preserve: {before:?}"
    );

    let (status, body) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Issue development", "edits": [{ "key": "inspect-issue", "goal": "Read the issue." }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let after = template_task_blocks(&boot, ISSUE_DEVELOPMENT).await;

    assert_eq!(before.len(), after.len());
    for (was, now) in before.iter().zip(&after) {
        for (field, value) in was.as_object().expect("task object") {
            if field == "goal" && was["key"] == "inspect-issue" {
                continue;
            }
            assert_eq!(
                now.get(field),
                Some(value),
                "field `{field}` changed on task {}: was={was} now={now}",
                was["key"]
            );
        }
        assert_eq!(
            was.as_object().unwrap().len(),
            now.as_object().unwrap().len(),
            "a field appeared or vanished on task {}",
            was["key"]
        );
    }
    assert_eq!(
        after
            .iter()
            .find(|task| task["key"] == "inspect-issue")
            .expect("task")["goal"],
        "Read the issue."
    );
}

/// Read the template wave's *stored* task blocks, not the endpoint's projection
/// — the projection is `key` + `goal`, and these tests are about the fields it
/// deliberately hides.
async fn template_task_blocks(boot: &Boot, key: &str) -> Vec<Value> {
    let wave_id = seeded_templates(&boot.repo)
        .await
        .into_iter()
        .find(|(template_key, _)| template_key == key)
        .map(|(_, wave_id)| wave_id)
        .unwrap_or_else(|| panic!("template `{key}` is not seeded"));
    let (_, detail) = get(boot.app.clone(), &format!("/api/waves/{wave_id}")).await;
    let payload = report_card_payload(&detail);
    task_blocks(&payload).into_iter().cloned().collect()
}

/// Appending is the one structural change the write endpoint allows.
#[tokio::test]
async fn a_task_can_be_appended_and_reaches_a_forked_wave() {
    let boot = boot().await;
    let uri = format!("/api/wave-templates/{INVESTIGATION}");
    let (status, saved) = put(
        boot.app.clone(),
        &uri,
        json!({
            "title": "Investigation",
            "appends": [{ "key": "hand-off", "goal": "Summarize the findings." }],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={saved}");
    assert_eq!(
        task_keys(&saved),
        vec!["gather-facts", "write-findings", "hand-off"]
    );

    // The appended block carries the server's own shape, not a bare pair.
    let blocks = template_task_blocks(&boot, INVESTIGATION).await;
    let appended = blocks
        .iter()
        .find(|task| task["key"] == "hand-off")
        .expect("appended block");
    assert_eq!(appended["declared_by"], "user");
    assert_eq!(appended["ready"], false);
    assert!(
        appended["no_gate_reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "an appended task without a no_gate_reason reads as scheduled work missing a gate: {appended}"
    );

    let (status, created) = post(
        boot.app.clone(),
        "/api/waves",
        create_body(
            &boot.cove_id,
            "forked-after-append",
            json!({ "template_id": INVESTIGATION }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={created}");
    let wave_id = created["id"].as_str().expect("wave id");
    let (_, detail) = get(boot.app.clone(), &format!("/api/waves/{wave_id}")).await;
    let payload = report_card_payload(&detail);
    let forked: Vec<&str> = task_blocks(&payload)
        .iter()
        .map(|block| block["key"].as_str().expect("key"))
        .collect();
    assert!(forked.contains(&"hand-off"), "got {forked:?}");
}

/// The write endpoint refuses what would render a broken plan. The last case is the
/// positive control, so these are not passing because it refuses everything.
#[tokio::test]
async fn a_save_that_would_render_a_broken_plan_is_refused() {
    let boot = boot().await;
    let uri = format!("/api/wave-templates/{SMALL_CHANGE}");
    for (case, body, expected) in [
        (
            "blank title",
            json!({ "title": "   ", "edits": [] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "blank goal",
            json!({ "title": "Small change", "edits": [{ "key": "inspect", "goal": "  " }] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "edit names a key the template does not declare",
            json!({ "title": "Small change", "edits": [{ "key": "nope", "goal": "x" }] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "append collides with an existing key",
            json!({ "title": "Small change", "appends": [{ "key": "inspect", "goal": "x" }] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "append key is malformed",
            json!({ "title": "Small change", "appends": [{ "key": "Not A Key", "goal": "x" }] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "two appends collide with each other",
            json!({ "title": "Small change", "appends": [
                { "key": "twice", "goal": "a" }, { "key": "twice", "goal": "b" },
            ] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "positive control: a plain goal edit",
            json!({ "title": "Small change", "edits": [{ "key": "inspect", "goal": "Look." }] }),
            StatusCode::OK,
        ),
    ] {
        let (status, response) = put(boot.app.clone(), &uri, body).await;
        assert_eq!(status, expected, "{case}: body={response}");
    }

    let (status, response) = put(
        boot.app.clone(),
        "/api/wave-templates/not-a-template",
        json!({ "title": "x", "edits": [] }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body={response}");
}

/// Review round 2, measured rather than argued.
///
/// With the old list-shaped body these were **accepted and persisted**:
/// `released_by_user: true` and `spawn: "sub-wave"`. Under the diff shape they
/// are not rejected — they are unexpressible, because the request has nowhere
/// to put them. This test pins that by sending them anyway and asserting they
/// reach no stored block.
#[tokio::test]
async fn privileged_task_vocabulary_is_refused_by_the_request_shape() {
    let boot = boot().await;
    let uri = format!("/api/wave-templates/{SMALL_CHANGE}");

    // The guarantee is "there is nowhere to put these", so the request must be
    // *rejected*, not sanitised. Asserting only that the stored block came out
    // clean was the weaker claim: `ready` / `declared_by` are restamped
    // unconditionally, so two of those assertions were green regardless of the
    // request, and the rest rested on a struct whose closedness nothing tested.
    for (case, body) in [
        (
            "privileged vocabulary on an edit",
            json!({ "title": "Small change", "edits": [
                { "key": "inspect", "goal": "Look.", "released_by_user": true },
            ] }),
        ),
        (
            "spawn on an append",
            json!({ "title": "Small change", "appends": [
                { "key": "sneaky", "goal": "Do.", "spawn": "sub-wave" },
            ] }),
        ),
        (
            "a tombstone smuggled into an append",
            json!({ "title": "Small change", "appends": [
                { "key": "sneaky", "goal": "Do.", "tombstone": { "reason": null } },
            ] }),
        ),
        (
            "an unknown top-level field",
            json!({ "title": "Small change", "edits": [], "tasks": [] }),
        ),
    ] {
        let (status, response) = put(boot.app.clone(), &uri, body).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{case}: the request shape must refuse this outright: body={response}"
        );
    }

    // Positive control: the same request without the extra key is accepted, so
    // the refusals are about the extra field and not about the shape at large.
    let (status, response) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Small change", "edits": [{ "key": "inspect", "goal": "Look." }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "positive control: body={response}");

    // And nothing privileged reached a stored block either.
    for task in template_task_blocks(&boot, SMALL_CHANGE).await {
        assert_ne!(task["released_by_user"], json!(true), "task={task}");
        assert_ne!(task["spawn"], json!("sub-wave"), "task={task}");
        assert_eq!(task["declared_by"], "user", "task={task}");
        assert_eq!(task["ready"], false, "task={task}");
    }
}

/// A tombstone must survive every save, and the retired key must stay retired.
///
/// The old list-shaped body let a client erase a tombstone by omitting it —
/// `guard_task_declarations`' removal check is gated on `!is_tombstone(old)`, so
/// nothing refused it — and then re-append the key, reversing a #1179-governed
/// deletion. The diff shape cannot express an omission at all.
#[tokio::test]
async fn a_tombstone_survives_saves_and_its_key_stays_retired() {
    let boot = boot().await;
    let uri = format!("/api/wave-templates/{SMALL_CHANGE}");
    let (status, body) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Small change", "edits": [] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seed: body={body}");

    // Retire `verify` the way the ordinary report editor would: a user delete,
    // which `normalize_report_op` rewrites into an in-place tombstone.
    let wave_id = seeded_templates(&boot.repo)
        .await
        .into_iter()
        .find(|(key, _)| key == SMALL_CHANGE)
        .map(|(_, id)| id)
        .expect("seeded");
    tombstone_task(&boot, &wave_id, "verify").await;
    assert!(
        template_task_blocks(&boot, SMALL_CHANGE)
            .await
            .iter()
            .any(|task| task["key"] == "verify" && task.get("tombstone").is_some()),
        "precondition: `verify` is tombstoned"
    );

    // A save that does not mention it at all must not remove it.
    let (status, body) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Renamed", "edits": [{ "key": "inspect", "goal": "Look harder." }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let blocks = template_task_blocks(&boot, SMALL_CHANGE).await;
    let tomb = blocks
        .iter()
        .find(|task| task["key"] == "verify")
        .unwrap_or_else(|| panic!("the tombstone was erased by an unrelated save: {blocks:?}"));
    assert!(
        tomb.get("tombstone").is_some(),
        "resurrected in place: {tomb}"
    );
    assert_eq!(tomb["tombstoned_by"], "user");

    // The picker must not advertise it…
    let (_, listed) = get(boot.app.clone(), "/api/wave-templates").await;
    assert!(!task_keys(listed_template(&listed, SMALL_CHANGE)).contains(&"verify"));

    // …and the key must stay retired: an append may not reuse it.
    let (status, response) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Renamed", "appends": [{ "key": "verify", "goal": "Back from the dead." }] }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a retired key must not be reusable: body={response}"
    );
}

/// Retire a task through the **production** delete path, not by hand-writing a
/// tombstone: `DELETE /api/waves/{id}/report/blocks/{block_id}`, which
/// `normalize_report_op` rewrites into an in-place tombstone for a `User`
/// author. A hand-made tombstone would prove nothing about the shape the
/// system actually produces.
async fn tombstone_task(boot: &Boot, wave_id: &str, key: &str) {
    // This harness applies only `actor_middleware`, so the report routes'
    // `Principal` extractor has nothing to read. Inject one rather than
    // hand-writing a tombstone fence: the shape a tombstone has is exactly what
    // is under test, and `normalize_report_op` is the only thing that should
    // decide it.
    let authed = authed_router(boot);

    let (status, report) = get(authed.clone(), &format!("/api/waves/{wave_id}/report")).await;
    assert_eq!(status, StatusCode::OK, "read report: {report}");
    let block = report["blocks"]
        .as_array()
        .unwrap_or_else(|| panic!("report has no blocks array: {report}"))
        .iter()
        .find(|block| block["kind"] == "task" && block["payload"]["key"] == key)
        .unwrap_or_else(|| panic!("no task block for `{key}` in {report}"));
    let block_id = block["id"].as_str().expect("block id").to_string();
    let rev = block["rev"].as_u64().expect("block rev");

    let resp = authed
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/waves/{wave_id}/report/blocks/{block_id}"))
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "user")
                .body(Body::from(json!({ "ifBlockRev": rev }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::OK, "delete block: {body}");
}

/// Round-3 finding, reproduced before any fix: the save rewrites the whole body
/// with `WriteMarkdown` and emits **no `<!-- neige:b_xxxx -->` markers**, so
/// block identity is decided by `align.rs`'s similarity heuristic even though
/// the handler knows exactly which stored block each payload came from.
///
/// Editing two goals in one save — which the editor's single Save button makes
/// the ordinary case — with one of them replaced by much longer text drops the
/// similarity below the reuse threshold, the old block goes unassigned, and the
/// guard reports it as a deletion.
#[tokio::test]
async fn editing_two_goals_at_once_with_a_long_replacement_is_savable() {
    let boot = boot().await;
    let uri = format!("/api/wave-templates/{SMALL_CHANGE}");
    let (status, body) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Small change", "edits": [] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seed: body={body}");

    // Several shapes, each an ordinary thing a person does in this editor.
    for (case, edits) in [
        (
            "two adjacent goals, one replaced by much longer text",
            json!([
                { "key": "inspect", "goal": "x".repeat(2000) },
                { "key": "implement", "goal": "Implement it and commit." },
            ]),
        ),
        (
            "all three goals replaced with unrelated short text",
            json!([
                { "key": "inspect", "goal": "a" },
                { "key": "implement", "goal": "b" },
                { "key": "verify", "goal": "c" },
            ]),
        ),
        (
            "two adjacent goals swapped in content",
            json!([
                { "key": "inspect", "goal": "Run the repository's standard tests and record the result." },
                { "key": "implement", "goal": "Read the requested change and the current code that it touches." },
            ]),
        ),
    ] {
        let (status, response) = put(
            boot.app.clone(),
            &uri,
            json!({ "title": "Small change", "edits": edits }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{case}: body={response}");
    }
}

/// Round-3 finding: rebuilding the body from the task fences alone dropped
/// every other block. The rebuild now walks the report's **blocks** and
/// re-emits each with its `<!-- neige:b_xxxx -->` marker, so nothing is lost
/// and the aligner is handed identity rather than made to guess it.
#[tokio::test]
async fn a_save_preserves_blocks_it_does_not_edit() {
    let boot = boot().await;
    let uri = format!("/api/wave-templates/{SMALL_CHANGE}");
    // Seed through wave *creation*, not through `PUT`. Seeding with a save
    // would run the very code under test before `before` is captured, and then
    // `before == after` holds no matter what the save does — the first version
    // of this test did exactly that and both mutations passed it.
    let (status, created) = post(
        boot.app.clone(),
        "/api/waves",
        create_body(
            &boot.cove_id,
            "seed-for-preserve",
            json!({ "template_id": SMALL_CHANGE }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "seed: body={created}");

    let before = template_blocks(&boot, SMALL_CHANGE).await;
    let kinds_before: Vec<String> = before
        .iter()
        .map(|b| b["kind"].as_str().unwrap().to_string())
        .collect();
    let ids_before: Vec<String> = before
        .iter()
        .map(|b| b["id"].as_str().unwrap().to_string())
        .collect();
    assert!(
        kinds_before.iter().any(|kind| kind != "task"),
        "fixture must carry a non-task block or this proves nothing: {kinds_before:?}"
    );

    let (status, body) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Small change", "edits": [{ "key": "inspect", "goal": "Look." }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    let after = template_blocks(&boot, SMALL_CHANGE).await;
    assert_eq!(
        after
            .iter()
            .map(|b| b["kind"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        kinds_before,
        "a save dropped or added a block"
    );
    // Ids preserved is the marker's whole job: without it the aligner re-derives
    // identity from text similarity and can mint new ids for edited blocks.
    assert_eq!(
        after
            .iter()
            .map(|b| b["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        ids_before,
        "block ids changed across a save — the markers are not doing their job"
    );
}

/// Editing a retired task used to return 200 and do nothing: the projection
/// drops tombstones, so no client could tell success from silent no-op.
#[tokio::test]
async fn editing_a_retired_task_is_refused_rather_than_silently_dropped() {
    let boot = boot().await;
    let uri = format!("/api/wave-templates/{SMALL_CHANGE}");
    let (status, body) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Small change", "edits": [] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seed: body={body}");
    let wave_id = seeded_templates(&boot.repo)
        .await
        .into_iter()
        .find(|(key, _)| key == SMALL_CHANGE)
        .map(|(_, id)| id)
        .expect("seeded");
    tombstone_task(&boot, &wave_id, "verify").await;

    let (status, response) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Small change", "edits": [{ "key": "verify", "goal": "back" }] }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={response}");

    // Positive control: a live key in the same shape is accepted, so the
    // refusal is about retirement and not about edits in general.
    let (status, response) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Small change", "edits": [{ "key": "inspect", "goal": "Look." }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={response}");
}

/// The same key edited twice in one request is a client bug, not a last-wins.
#[tokio::test]
async fn the_same_key_edited_twice_in_one_save_is_refused() {
    let boot = boot().await;
    let (status, response) = put(
        boot.app.clone(),
        &format!("/api/wave-templates/{SMALL_CHANGE}"),
        json!({ "title": "Small change", "edits": [
            { "key": "inspect", "goal": "a" }, { "key": "inspect", "goal": "b" },
        ] }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={response}");
}

/// Read the template wave's blocks with ids and kinds.
async fn template_blocks(boot: &Boot, key: &str) -> Vec<Value> {
    let wave_id = seeded_templates(&boot.repo)
        .await
        .into_iter()
        .find(|(template_key, _)| template_key == key)
        .map(|(_, wave_id)| wave_id)
        .unwrap_or_else(|| panic!("template `{key}` is not seeded"));
    let (_, detail) = get(boot.app.clone(), &format!("/api/waves/{wave_id}")).await;
    let card = detail["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["kind"] == "wave-report")
        .expect("wave-report card");
    card["payload"]["blocks"]
        .as_array()
        .expect("blocks")
        .clone()
}

/// Round-4 self-check: the write endpoint and the create path must agree on which ids
/// are templates. #1209 PR-1 made `workflow_template()` the single roster
/// lookup; this asserts the two callers actually land on the same answer rather
/// than each keeping a private judgement.
#[tokio::test]
async fn the_write_endpoint_and_create_admit_exactly_the_same_ids() {
    let boot = boot().await;
    let (status, listed) = get(boot.app.clone(), "/api/wave-templates").await;
    assert_eq!(status, StatusCode::OK, "body={listed}");
    let ids: Vec<String> = listed
        .as_array()
        .expect("array")
        .iter()
        .map(|entry| entry["id"].as_str().expect("id").to_string())
        .collect();
    assert!(
        !ids.is_empty(),
        "the read endpoint listed nothing to compare"
    );

    for id in &ids {
        let (write, _) = put(
            boot.app.clone(),
            &format!("/api/wave-templates/{id}"),
            json!({ "title": "t", "edits": [] }),
        )
        .await;
        assert_eq!(
            write,
            StatusCode::OK,
            "listed id `{id}` refused by the write endpoint"
        );
    }

    // …and an id neither knows is refused by both, with the same 404-vs-400
    // split each path documents.
    for unknown in ["not-a-template", "issue-development-x", ""] {
        let (write, _) = put(
            boot.app.clone(),
            &format!("/api/wave-templates/{unknown}"),
            json!({ "title": "t", "edits": [] }),
        )
        .await;
        assert!(
            write == StatusCode::NOT_FOUND || write == StatusCode::METHOD_NOT_ALLOWED,
            "unknown id `{unknown}` reached the write endpoint with {write}"
        );
        let (create, _) = post(
            boot.app.clone(),
            "/api/waves",
            create_body(
                &boot.cove_id,
                &format!("unknown-{unknown}"),
                json!({ "template_id": unknown }),
            ),
        )
        .await;
        assert_ne!(
            create,
            StatusCode::CREATED,
            "create accepted unknown id `{unknown}`"
        );
    }
}

/// Round-4 self-check: a title is user text and lands in the report *summary*,
/// not the body — but assert it rather than assume, because a title that could
/// reach the body could close the contract comment (`-->`) or forge a block id
/// marker.
#[tokio::test]
async fn a_hostile_title_cannot_reach_the_report_body() {
    let boot = boot().await;
    let uri = format!("/api/wave-templates/{SMALL_CHANGE}");
    let hostile = "--> <!-- neige:b_dead --> # 概要";
    let (status, body) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": hostile, "edits": [] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    let blocks = template_blocks(&boot, SMALL_CHANGE).await;
    for block in &blocks {
        let text = serde_json::to_string(block).expect("block json");
        assert!(
            !text.contains("b_dead"),
            "a title reached a block and could forge an id: {block}"
        );
    }
    // The title is the summary, and it is stored verbatim there.
    let (_, listed) = get(boot.app.clone(), "/api/wave-templates").await;
    assert_eq!(listed_template(&listed, SMALL_CHANGE)["title"], hostile);
}

/// Round-4 self-check: duplicate live keys are representable in a report
/// (`dup_keys` is a diagnostic, not a write-time refusal), and an edit naming
/// one used to rewrite **both** blocks with a single goal — a coincidence, not
/// a decision. It is now refused.
#[tokio::test]
async fn an_edit_on_an_ambiguous_duplicate_key_is_refused() {
    let boot = boot().await;
    let uri = format!("/api/wave-templates/{SMALL_CHANGE}");
    let (status, body) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Small change", "edits": [] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seed: body={body}");
    let wave_id = seeded_templates(&boot.repo)
        .await
        .into_iter()
        .find(|(key, _)| key == SMALL_CHANGE)
        .map(|(_, id)| id)
        .expect("seeded");

    // A second live block declaring `inspect`, created through the real block
    // write路 — not hand-written into the body, because whether the system can
    // even *produce* this state is part of what the test is about.
    add_duplicate_task_block(&boot, &wave_id, "inspect").await;

    let (status, response) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Small change", "edits": [{ "key": "inspect", "goal": "Which one?" }] }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={response}");

    // Positive control: an unambiguous key in the same request shape still works.
    let (status, response) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Small change", "edits": [{ "key": "implement", "goal": "Do it." }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={response}");
}

/// Create a second live `task` block declaring `key`, through the report block
/// write path (`POST /api/waves/{id}/report/blocks`).
async fn add_duplicate_task_block(boot: &Boot, wave_id: &str, key: &str) {
    let authed = authed_router(boot);
    let (status, report) = get(authed.clone(), &format!("/api/waves/{wave_id}/report")).await;
    assert_eq!(status, StatusCode::OK, "read report: {report}");
    let doc_rev = report["docRev"]
        .as_u64()
        .or_else(|| report["doc_rev"].as_u64())
        .unwrap_or_else(|| panic!("no doc rev in {report}"));

    let resp = authed
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/waves/{wave_id}/report/blocks"))
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "user")
                .body(Body::from(
                    json!({
                        "kind": "task",
                        "payload": {
                            "key": key, "kind": "codex", "goal": "A second declaration.",
                            "ready": false, "declared_by": "user",
                        },
                        "ifDocRev": doc_rev,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::OK, "create duplicate block: {body}");
}

/// The test router with a `Principal` in request extensions.
///
/// This harness applies only `actor_middleware`, so the report routes'
/// `Principal` extractor has nothing to read. Injecting one lets these tests
/// drive the **real** block write/delete routes rather than hand-writing the
/// block shapes under test — `normalize_report_op` is the only thing that
/// should decide what a tombstone looks like.
fn authed_router(boot: &Boot) -> axum::Router {
    routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .layer(axum::middleware::from_fn(
            |mut request: axum::extract::Request, next: axum::middleware::Next| async move {
                request
                    .extensions_mut()
                    .insert(calm_server::auth::Principal {
                        user_id: "owner".into(),
                        display_name: "Owner".into(),
                        role: "owner".into(),
                        session_id: "test-session".into(),
                    });
                next.run(request).await
            },
        ))
        .with_state(boot.state.clone())
}

/// Round-4 finding: a save must be **idempotent**. The rebuild emits
/// `marker + text` per block, and each block's text already carries its own
/// separator — an extra unconditional newline made the body grow by one byte
/// per block on every save, including one that changes nothing, churning every
/// block's `rev` and turning the single blank line between two fences into a
/// widening gap.
#[tokio::test]
async fn a_no_op_save_leaves_the_body_byte_identical() {
    let boot = boot().await;
    let uri = format!("/api/wave-templates/{SMALL_CHANGE}");
    let (status, body) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Small change", "edits": [] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seed: body={body}");

    let before = template_report_body(&boot, SMALL_CHANGE).await;
    for round in 0..3 {
        let (status, body) = put(
            boot.app.clone(),
            &uri,
            json!({ "title": "Small change", "edits": [] }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "round {round}: body={body}");
        let after = template_report_body(&boot, SMALL_CHANGE).await;
        assert_eq!(
            after.len(),
            before.len(),
            "round {round}: the body grew by {} bytes on a save that changed nothing",
            after.len() as i64 - before.len() as i64
        );
        assert_eq!(
            after, before,
            "round {round}: the body changed on a no-op save"
        );
    }
}

async fn template_report_body(boot: &Boot, key: &str) -> String {
    let wave_id = seeded_templates(&boot.repo)
        .await
        .into_iter()
        .find(|(template_key, _)| template_key == key)
        .map(|(_, wave_id)| wave_id)
        .unwrap_or_else(|| panic!("template `{key}` is not seeded"));
    let (_, detail) = get(boot.app.clone(), &format!("/api/waves/{wave_id}")).await;
    report_card_payload(&detail).body
}

// ---------------------------------------------------------------------------
// #1209 PR-2 test #16 — the write side knows exactly one spelling.
// ---------------------------------------------------------------------------

/// Shared body of the three legs below. Asserts the request is rejected at the
/// **serde extractor**: `CreateWaveRequest` carries
/// `#[serde(deny_unknown_fields)]`, so the pre-rename key is an unknown field
/// and the handler is never entered — `admit_template` does not run.
///
/// **Status is 422, not 400.** The #1209 design predicted 400; the observed
/// behaviour is axum's `JsonRejection::JsonDataError`, which is
/// `422 Unprocessable Entity` with a plain-text body. The ruling the design
/// actually makes still holds — "reject loudly at the serde layer, do not
/// declare the old key as a field" — and the status code is a consequence of
/// the extractor, not something this slice chose. It is pinned here rather
/// than customised, because customising it would mean declaring `workflow_id`
/// on `CreateWaveRequest`, i.e. reintroducing the writeable alias #1209
/// rejects.
///
/// The assertions therefore look for serde's own wording and explicitly
/// require the admission wording to be **absent**: an admission-flavoured
/// error would mean the old key had become a declared field again.
async fn assert_old_spelling_is_an_unknown_field(leg: &str, body_json: Value, unknown_key: &str) {
    let boot = boot().await;
    let before = db_snapshot(&boot.repo).await;
    let (status, text) = post_text(boot.app.clone(), "/api/waves", body_json).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{leg}: body={text}"
    );
    assert!(
        text.contains("unknown field"),
        "{leg}: expected a serde unknown-field rejection, body={text}"
    );
    assert!(
        text.contains(unknown_key),
        "{leg}: rejection must name `{unknown_key}`, body={text}"
    );
    assert!(
        !text.contains("known wave template"),
        "{leg}: the old spelling must not reach `admit_template` — reaching it \
         means `CreateWaveRequest` declares the old key again, body={text}"
    );
    assert_eq!(
        db_snapshot(&boot.repo).await,
        before,
        "{leg}: a rejected create must not write"
    );
}

/// #1209 PR-2 (design §3.5, matrix row 18) — the pre-rename `template_id`
/// spelling.
///
/// This is the only pin on the whole rejection policy: the request body is a
/// live contract, so the old spelling must fail loudly rather than be silently
/// accepted through an alias. Mutation: give `CreateWaveRequest` back a
/// `workflow_id` field — even as a bare `#[serde(alias)]` — and this goes red
/// (the request would then be accepted, so both the status and the body
/// assertion fail).
///
/// Three separate tests, not a loop over three bodies: a loop stops at the
/// first failure, so the later legs would never be shown to discriminate.
#[tokio::test]
async fn old_template_id_spelling_is_an_unknown_field() {
    assert_old_spelling_is_an_unknown_field(
        "row 18: workflow_id alone",
        json!({
            "cove_id": "",
            "title": "old id spelling",
            "attach_folder": false,
            "theme": theme(),
            "workflow_id": SMALL_CHANGE,
        }),
        "workflow_id",
    )
    .await;
}

/// #1209 PR-2 (design §3.5, matrix row 19) — the pre-rename `template_input`
/// spelling, paired with the *new* `template_id`. Half-migrated callers must
/// fail too; accepting this shape would be the "partially works" outcome
/// `docs/upgrade-stability.md` forbids.
#[tokio::test]
async fn old_template_input_spelling_is_an_unknown_field() {
    assert_old_spelling_is_an_unknown_field(
        "row 19: new template_id + old workflow_input",
        json!({
            "cove_id": "",
            "title": "old input spelling",
            "attach_folder": false,
            "theme": theme(),
            "template_id": SMALL_CHANGE,
            "workflow_input": { "issue_url": "https://example.invalid/1" },
        }),
        "workflow_input",
    )
    .await;
}

/// #1209 PR-2 (design §3.5, matrix row 20) — both spellings at once.
///
/// This leg is what pins "the write side knows exactly ONE name". Rows 18/19
/// would both stay green under an implementation that accepted either spelling
/// but not both; only this one fails if the rejected option B (a writeable
/// alias) comes back through the side door.
#[tokio::test]
async fn both_spellings_together_are_an_unknown_field() {
    assert_old_spelling_is_an_unknown_field(
        "row 20: template_id and workflow_id together",
        json!({
            "cove_id": "",
            "title": "both spellings",
            "attach_folder": false,
            "theme": theme(),
            "template_id": SMALL_CHANGE,
            "workflow_id": SMALL_CHANGE,
        }),
        "workflow_id",
    )
    .await;
}
