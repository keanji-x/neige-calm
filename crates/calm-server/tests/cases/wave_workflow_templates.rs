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
// #1230 — editable templates, via a diff write口.
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

/// Appending is the one structural change the write口 allows.
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
            json!({ "workflow_id": INVESTIGATION }),
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

/// The write口 refuses what would render a broken plan. The last case is the
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
    let authed = routes::router()
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
        .with_state(boot.state.clone());

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
            json!({ "workflow_id": SMALL_CHANGE }),
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
