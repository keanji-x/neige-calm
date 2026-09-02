//! #1110 S6 — seed template waves; auto-fork on create.
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
            name: "template-template-test".into(),
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
            "unknown-template",
            json!({ "template_id": "missing-template" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    // #1209 — three legs, because the interesting regression is not "some 400
    // happened" but "the 400 was decided by the roster". Leg 2 is the one that
    // catches restoring the registry wording (and with it the registry as the
    // admission authority); a `.contains("missing-template")`-only assertion
    // was green both before and after that change.
    let error = body["error"].as_str().unwrap_or("");
    assert!(error.contains("known wave template"), "body={body}");
    assert!(
        !error.contains("registered trusted template"),
        "body={body}"
    );
    assert!(error.contains("missing-template"), "body={body}");
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
            name: "template-template-plugin-test".into(),
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

    let templates: Vec<Value> = declared_template_ids
        .iter()
        .map(|id| json!({ "id": id }))
        .collect();
    let manifest = Manifest::parse(
        &json!({
            "manifest_version": 2,
            "id": plugin_id,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Trusted template owner",
            "entrypoint": { "command": "bin/stub" },
            // No `input_schema`: see this function's doc comment, point 1.
            "templates": templates,
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
/// A running, trusted plugin declares a template id that is not in the kernel's
/// template roster. Before #1209 that made the id creatable (201, with
/// `plugin_scope` stamped and nothing to fork); the create path asked the
/// plugin registry first and only consulted the roster as a fallback. #1209
/// inverts that: the roster is the admission test and the binding is an
/// attribute, so this create is a 400 — and the plugin's running/trusted state
/// cannot change that answer.
///
/// The mutation this must catch is restoring the fallback (an
/// `.or_else(|| resolve_template_binding(..))` inside `admit_template`, in any
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
        !error.contains("registered trusted template"),
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
/// running trusted plugin, `resolve_template_binding` is `None` for every id,
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
/// (`template_by_key`), plus test #8 for the one concrete shape that
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
/// Mutations that must turn this red: calling `ensure_templates` (or
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
/// sends `missing-template` and stays green through that change; this one does
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
            "title": "blank template id",
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
// #1230 — the template picker read.
//
// #1230 also added a diff write endpoint (`PUT /api/wave-templates/{id}`) and
// a Settings editor on top of it. #1300 S1 removed both: they were built on
// the seeded template wave, which #1300 removes because it is the last
// production path on which the kernel writes a report as `EditAuthor::User`.
// The assertion that the route is gone (and wrote nothing on its way out)
// lives in `wave_templates_read.rs::put_is_not_routed_and_writes_nothing`.
// ---------------------------------------------------------------------------

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
