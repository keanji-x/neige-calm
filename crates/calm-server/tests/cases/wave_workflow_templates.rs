//! #1110 S6 — seed workflow template waves; auto-fork on create.
//!
//! Matching `workflow_id` lazily seeds three system-cove template waves
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
use calm_server::model::NewCove;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
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
async fn matching_workflow_id_seeds_one_wave_per_template_key() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        create_body(
            &boot.cove_id,
            "first-issue-dev",
            json!({ "workflow_id": ISSUE_DEVELOPMENT }),
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
            json!({ "workflow_id": SMALL_CHANGE }),
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
            json!({ "workflow_id": ISSUE_DEVELOPMENT }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    assert_eq!(body["workflow_id"], ISSUE_DEVELOPMENT);
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
                "workflow_id": ISSUE_DEVELOPMENT,
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
                json!({ "workflow_id": key }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "key={key} body={body}");
        assert_eq!(body["workflow_id"], key);
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
            json!({ "workflow_id": ISSUE_DEVELOPMENT }),
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
async fn unknown_workflow_id_still_400s() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app,
        "/api/waves",
        create_body(
            &boot.cove_id,
            "unknown-workflow",
            json!({ "workflow_id": "missing-workflow" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("must reference a registered trusted workflow"),
        "body={body}"
    );
}

// ---------------------------------------------------------------------------
// #1230 — editable templates.
//
// The three tests below hold the one property the feature is for: after a save,
// the New wave picker and the wave the create path actually produces say the
// same thing. Asserting only one of the two would pass while the other drifted,
// which is precisely the pre-#1230 state.
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
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

fn listed_template<'a>(body: &'a Value, id: &str) -> &'a Value {
    body.as_array()
        .expect("array body")
        .iter()
        .find(|entry| entry["id"] == id)
        .unwrap_or_else(|| panic!("template `{id}` missing from {body}"))
}

/// Acceptance 2 — a read of the picker must not mint the template waves.
///
/// The count assertion is the whole test: `GET /api/wave-templates` returning
/// the right constants proves nothing on its own, because the seeding path
/// would also return the right values. What must hold is that the read left the
/// database exactly as it found it.
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
        listed_template(&body, SMALL_CHANGE)["tasks"][0]["key"],
        "inspect"
    );

    // Same for the per-template definition read.
    let (status, definition) = get(
        boot.app.clone(),
        &format!("/api/wave-templates/{SMALL_CHANGE}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={definition}");
    assert_eq!(definition["seeded"], false);
    assert_eq!(definition["title"], "Small change");

    assert!(
        seeded_templates(&boot.repo).await.is_empty(),
        "a GET must never trigger the lazy seed"
    );
}

/// Acceptance 1 — edit a goal, then check *both* readers.
#[tokio::test]
async fn an_edited_goal_reaches_the_picker_and_the_forked_wave() {
    let boot = boot().await;
    const NEW_GOAL: &str = "Read the request and write down what it touches, then stop.";
    const NEW_TITLE: &str = "Tiny change";

    let (status, definition) = get(
        boot.app.clone(),
        &format!("/api/wave-templates/{SMALL_CHANGE}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={definition}");
    let mut tasks = definition["tasks"].as_array().expect("tasks").clone();
    assert_eq!(tasks[0]["key"], "inspect");
    tasks[0]["goal"] = json!(NEW_GOAL);

    let (status, saved) = put(
        boot.app.clone(),
        &format!("/api/wave-templates/{SMALL_CHANGE}"),
        json!({ "title": NEW_TITLE, "tasks": tasks }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={saved}");
    assert_eq!(saved["seeded"], true);
    assert_eq!(saved["title"], NEW_TITLE);
    assert_eq!(saved["tasks"][0]["goal"], NEW_GOAL);

    // Reader 1: the picker.
    let (status, body) = get(boot.app.clone(), "/api/wave-templates").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let listed = listed_template(&body, SMALL_CHANGE);
    assert_eq!(listed["title"], NEW_TITLE);
    assert_eq!(listed["tasks"][0]["goal"], NEW_GOAL);
    // The untouched templates did not move.
    assert_eq!(
        listed_template(&body, INVESTIGATION)["title"],
        "Investigation"
    );

    // Reader 2: the wave the create path actually produces. This is the half
    // that pre-#1230 already worked and that the picker used to contradict.
    let (status, created) = post(
        boot.app.clone(),
        "/api/waves",
        create_body(
            &boot.cove_id,
            "forked-after-edit",
            json!({ "workflow_id": SMALL_CHANGE }),
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

/// The fields Settings does not display must survive a save.
///
/// The editor round-trips whole task objects precisely so that this holds; a
/// handler that rebuilt tasks from `key` + `goal` would pass the goal test
/// above and silently flatten every dependency edge and acceptance criterion.
#[tokio::test]
async fn saving_preserves_the_task_fields_the_editor_does_not_show() {
    let boot = boot().await;
    let (status, before) = get(
        boot.app.clone(),
        &format!("/api/wave-templates/{ISSUE_DEVELOPMENT}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={before}");
    let mut tasks = before["tasks"].as_array().expect("tasks").clone();
    // Sanity: the fixture actually carries the fields we claim to preserve.
    assert!(tasks.iter().any(|task| {
        task["depends_on"]
            .as_array()
            .is_some_and(|deps| !deps.is_empty())
    }));
    assert!(tasks.iter().any(|task| task.get("context").is_some()));
    // Task-block vocabulary, not `PlanTaskInput` vocabulary: since the route
    // reads whole payloads, the field is spelled `acceptance` here — the name
    // `plan_template_task_block_payload` renders it under.
    assert!(tasks.iter().any(|task| task.get("acceptance").is_some()));

    // Touch one goal — the smallest edit a user can make.
    tasks[0]["goal"] = json!("Read the issue.");
    let (status, saved) = put(
        boot.app.clone(),
        &format!("/api/wave-templates/{ISSUE_DEVELOPMENT}"),
        json!({ "title": "Issue development", "tasks": tasks.clone() }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={saved}");

    let (status, after) = get(
        boot.app.clone(),
        &format!("/api/wave-templates/{ISSUE_DEVELOPMENT}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={after}");
    // Field-by-field against what was sent, ignoring the two stamps this route
    // owns (`ready` / `declared_by`). Whole-object equality would be asserting
    // the stamps too, which the client never sends.
    let stored = after["tasks"].as_array().expect("tasks");
    assert_eq!(stored.len(), tasks.len());
    for (sent, kept) in tasks.iter().zip(stored) {
        for (field, value) in sent.as_object().expect("task object") {
            if field == "ready" || field == "declared_by" {
                continue;
            }
            assert_eq!(
                kept.get(field),
                Some(value),
                "field `{field}` was not stored verbatim: sent={sent} kept={kept}"
            );
        }
    }
}

/// The write side refuses what would render a broken plan. Each case names a
/// distinct rejection reason, and the last one is the positive control: the
/// same request shape with the defect removed is accepted, so these are not
/// passing because the endpoint refuses everything.
#[tokio::test]
async fn a_save_that_would_render_a_broken_plan_is_refused() {
    let boot = boot().await;
    let uri = format!("/api/wave-templates/{SMALL_CHANGE}");
    // The positive control must be a task list the report contract will accept
    // on an *already seeded* template, so it keeps `small-change`'s three keys
    // and changes only their goals. Renaming or dropping a key is refused for a
    // separate, deeper reason — see
    // `renaming_or_removing_a_template_task_is_refused_by_the_report_contract`.
    let good = json!([
        { "key": "inspect", "kind": "codex", "goal": "Look." },
        { "key": "implement", "kind": "codex", "goal": "Do.", "depends_on": ["inspect"] },
        { "key": "verify", "kind": "codex", "goal": "Check.", "depends_on": ["implement"] },
    ]);

    for (case, body, expected) in [
        (
            "blank title",
            json!({ "title": "   ", "tasks": good }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "no tasks",
            json!({ "title": "Small change", "tasks": [] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "duplicate key",
            json!({ "title": "Small change", "tasks": [
                { "key": "inspect", "kind": "codex", "goal": "Look." },
                { "key": "inspect", "kind": "codex", "goal": "Look again." },
            ] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "dependency on a key the template does not declare",
            json!({ "title": "Small change", "tasks": [
                { "key": "inspect", "kind": "codex", "goal": "Look.", "depends_on": ["gone"] },
            ] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "invalid key syntax",
            json!({ "title": "Small change", "tasks": [
                { "key": "Not A Key", "kind": "codex", "goal": "Look." },
            ] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "blank goal",
            json!({ "title": "Small change", "tasks": [
                { "key": "inspect", "kind": "codex", "goal": "  " },
            ] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "positive control: the same shape, defect removed",
            json!({ "title": "Small change", "tasks": good }),
            StatusCode::OK,
        ),
    ] {
        let (status, response) = put(boot.app.clone(), &uri, body).await;
        assert_eq!(status, expected, "{case}: body={response}");
    }

    // An unknown key is a 404, not a 400 — and it must not have seeded either.
    let (status, response) = put(
        boot.app.clone(),
        "/api/wave-templates/not-a-template",
        json!({ "title": "x", "tasks": good }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body={response}");
}

/// #1230 / #1179 — the ceiling on template editing, recorded rather than
/// discovered twice.
///
/// A template's report is an ordinary wave report, so it is subject to the
/// task-declaration invariants in `wave_report_edit_guard`: a task block's
/// `key` is immutable for the life of that block, and a live task may only
/// leave a document through the block-level delete path (which, for a `User`
/// author, `normalize_report_op` rewrites into an in-place tombstone — and
/// `prepare_fork_report` then *copies* tombstones into every forked wave).
///
/// The practical consequence for Settings: goals and the other non-key fields
/// are editable, tasks can be appended, but a key cannot be renamed and a task
/// cannot be removed. That is a real product limit, not a bug in this handler,
/// and it must fail loudly at the save rather than half-apply.
#[tokio::test]
async fn renaming_or_removing_a_template_task_is_refused_by_the_report_contract() {
    let boot = boot().await;
    let uri = format!("/api/wave-templates/{SMALL_CHANGE}");
    let three = json!([
        { "key": "inspect", "kind": "codex", "goal": "Look." },
        { "key": "implement", "kind": "codex", "goal": "Do.", "depends_on": ["inspect"] },
        { "key": "verify", "kind": "codex", "goal": "Check.", "depends_on": ["implement"] },
    ]);
    // Seed by saving an accepted edit first: the limit only exists once the
    // task blocks are real blocks with ids.
    let (status, body) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Small change", "tasks": three }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "precondition save: body={body}");

    // Rename.
    let (status, body) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Small change", "tasks": [
            { "key": "inspect", "kind": "codex", "goal": "Look." },
            { "key": "renamed", "kind": "codex", "goal": "Do.", "depends_on": ["inspect"] },
            { "key": "verify", "kind": "codex", "goal": "Check.", "depends_on": ["renamed"] },
        ] }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "rename: body={body}");

    // Removal.
    let (status, body) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Small change", "tasks": [
            { "key": "inspect", "kind": "codex", "goal": "Look." },
        ] }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "removal: body={body}");

    // The refusals left the stored template exactly as the accepted save did:
    // a half-applied structural edit would be worse than the refusal.
    let (status, after) = get(boot.app.clone(), &uri).await;
    assert_eq!(status, StatusCode::OK, "body={after}");
    let keys: Vec<&str> = after["tasks"]
        .as_array()
        .expect("tasks")
        .iter()
        .map(|task| task["key"].as_str().expect("key"))
        .collect();
    assert_eq!(keys, vec!["inspect", "implement", "verify"]);
}

/// Appending is the structural change that *is* allowed, and the editor needs
/// it: a template you can only reword is not one you can extend.
#[tokio::test]
async fn a_task_can_be_appended_to_a_seeded_template() {
    let boot = boot().await;
    let uri = format!("/api/wave-templates/{INVESTIGATION}");
    let (status, before) = get(boot.app.clone(), &uri).await;
    assert_eq!(status, StatusCode::OK, "body={before}");
    let mut tasks = before["tasks"].as_array().expect("tasks").clone();
    tasks.push(json!({
        "key": "hand-off",
        "kind": "codex",
        "goal": "Summarize the findings for whoever picks this up.",
        "depends_on": ["write-findings"],
        "no_gate_reason": "a summary produces no repo change to verify",
    }));

    let (status, saved) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Investigation", "tasks": tasks }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={saved}");
    let keys: Vec<&str> = saved["tasks"]
        .as_array()
        .expect("tasks")
        .iter()
        .map(|task| task["key"].as_str().expect("key"))
        .collect();
    assert_eq!(keys, vec!["gather-facts", "write-findings", "hand-off"]);

    // And the appended task reaches a wave forked afterwards.
    let (status, created) = post(
        boot.app.clone(),
        "/api/waves",
        create_body(
            &boot.cove_id,
            "forked-after-append",
            json!({ "workflow_id": INVESTIGATION }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={created}");
    let wave_id = created["id"].as_str().expect("wave id");
    let (status, detail) = get(boot.app.clone(), &format!("/api/waves/{wave_id}")).await;
    assert_eq!(status, StatusCode::OK, "body={detail}");
    let payload = report_card_payload(&detail);
    let forked: Vec<&str> = task_blocks(&payload)
        .iter()
        .map(|block| block["key"].as_str().expect("key"))
        .collect();
    assert!(
        forked.contains(&"hand-off"),
        "the appended task must be forked too, got {forked:?}"
    );
}

/// #1239 review — a task block carrying vocabulary `PlanTaskInput` does not
/// model must survive the read *and* the write.
///
/// The first cut deserialized each payload into that struct, which is
/// `deny_unknown_fields`, so `refs` made a well-formed block vanish: the picker
/// under-reported the template, and the next save dropped a live task block and
/// was refused by the guard — leaving the template permanently unsavable from
/// Settings. This drives the whole route, not just the parser.
#[tokio::test]
async fn a_task_carrying_refs_survives_read_and_save() {
    let boot = boot().await;
    let uri = format!("/api/wave-templates/{SMALL_CHANGE}");

    // Read the seeded definition and *append* — replacing the seeded list would
    // trip the append-only limit, which is a different (already-tested) rule
    // and would mask the bug this test is about.
    let (status, seeded) = get(boot.app.clone(), &uri).await;
    assert_eq!(status, StatusCode::OK, "body={seeded}");
    let mut tasks = seeded["tasks"].as_array().expect("tasks").clone();
    tasks.push(json!({
        "key": "with-refs", "kind": "codex", "goal": "Consult the design.",
        "depends_on": ["inspect"],
        "refs": ["neige://wave/w1#b_0001"],
    }));
    let (status, body) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Small change", "tasks": tasks }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    // Read side: the task is listed, and its extra field round-tripped.
    let (status, definition) = get(boot.app.clone(), &uri).await;
    assert_eq!(status, StatusCode::OK, "body={definition}");
    let with_refs = definition["tasks"]
        .as_array()
        .expect("tasks")
        .iter()
        .find(|task| task["key"] == "with-refs")
        .unwrap_or_else(|| panic!("`with-refs` missing from {definition}"));
    assert_eq!(with_refs["refs"][0], "neige://wave/w1#b_0001");

    // Picker side: it is advertised too.
    let (status, listed) = get(boot.app.clone(), "/api/wave-templates").await;
    assert_eq!(status, StatusCode::OK, "body={listed}");
    let keys: Vec<&str> = listed_template(&listed, SMALL_CHANGE)["tasks"]
        .as_array()
        .expect("tasks")
        .iter()
        .map(|task| task["key"].as_str().expect("key"))
        .collect();
    assert!(keys.contains(&"with-refs"), "picker keys={keys:?}");

    // Write side: a save that changes only the title must not drop it, which is
    // the half the guard would have turned into a permanent 400.
    let mut tasks = definition["tasks"].as_array().expect("tasks").clone();
    let (status, saved) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Small change v2", "tasks": tasks }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a title-only save must not drop it: body={saved}"
    );

    // And the same for a *new* field the server has never seen in a constant.
    tasks[0]["priority"] = json!(3);
    let (status, saved) = put(
        boot.app.clone(),
        &uri,
        json!({ "title": "Small change v2", "tasks": tasks }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={saved}");
    let (status, after) = get(boot.app.clone(), &uri).await;
    assert_eq!(status, StatusCode::OK, "body={after}");
    assert_eq!(after["tasks"][0]["priority"], 3);
}
