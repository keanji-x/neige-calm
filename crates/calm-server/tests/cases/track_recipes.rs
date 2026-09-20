//! `/api/track-recipes`, user-defined starting points.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::common;

struct Boot {
    app: axum::Router,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = TrackAreaCache::new();
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
            std::env::temp_dir().join("calm-plugins-data-1292-s1"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                card_role_cache.clone(),
                track_area_cache.clone(),
            ),
        )),
        Arc::new(common::fake_codex_client()),
        Some(card_role_cache),
        Some(track_area_cache),
    );
    let shared = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let state = state.with_shared_codex_appserver(shared);
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);
    Boot { app, _tmp: tmp }
}

async fn send(
    app: axum::Router,
    method: &str,
    uri: &str,
    actor: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(actor) = actor {
        builder = builder.header("X-Calm-Actor", actor);
    }
    let request = match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn task_fence(payload: Value) -> String {
    format!(
        "```neige-block task\n{}\n```\n",
        serde_json::to_string_pretty(&payload).unwrap()
    )
}

/// Every task payload in a recipe body, in order.
fn task_payloads(body: &str) -> Vec<Value> {
    calm_types::report_blocks::split_body(body)
        .iter()
        .filter_map(|slice| calm_types::report_blocks::parse_fence(&slice.raw))
        .filter(|fence| fence.kind == calm_types::report_blocks::KIND_TASK)
        .map(|fence| fence.payload)
        .collect()
}

/// Built by the production formatter rather than spelled as a literal.
fn foreign_block_ref() -> String {
    calm_types::report_links::format_track_destination(FOREIGN_TRACK, Some("b_1f3a"))
}

const FOREIGN_TRACK: &str = "some-other-track";

/// Every parsed fence in a body, as `(kind, payload)`, in order.
fn fences(body: &str) -> Vec<(String, Value)> {
    calm_types::report_blocks::split_body(body)
        .iter()
        .filter_map(|slice| calm_types::report_blocks::parse_fence(&slice.raw))
        .map(|fence| (fence.kind, fence.payload))
        .collect()
}

/// `released_by_user` must be absent, not `false`: `track_report_edit_guard` compares the raw `Option<&Value>`.
#[tokio::test]
async fn create_normalizes_every_privilege_field_and_drops_tombstones() {
    let boot = boot().await;
    let body = format!(
        "# Plan\n\nintro\n\n{}{}",
        task_fence(json!({
            "key": "live",
            "goal": "do the thing",
            "kind": "codex",
            "declared_by": "user",
            "ready": true,
            "released_by_user": true,
        })),
        task_fence(json!({
            "key": "retired",
            "tombstone": { "reason": null },
            "declared_by": "user",
            "tombstoned_by": "user",
        })),
    );
    let (status, created) = send(
        boot.app.clone(),
        "POST",
        "/api/track-recipes",
        Some("user"),
        Some(json!({ "title": "mine", "body": body })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={created}");

    let stored = created["body"].as_str().expect("body");
    let tasks = task_payloads(stored);
    assert_eq!(tasks.len(), 1, "the tombstone must be gone: {tasks:?}");
    assert_eq!(tasks[0]["key"], json!("live"));
    assert_eq!(tasks[0]["declared_by"], json!("spec"));
    assert_eq!(tasks[0]["ready"], json!(false));
    assert!(
        tasks[0].get("released_by_user").is_none(),
        "must be absent, not false: {:?}",
        tasks[0]
    );
    assert!(
        stored.contains("intro"),
        "prose must survive verbatim: {stored}"
    );
}

#[tokio::test]
async fn create_drops_refs_and_touches_nothing_beside_them() {
    let boot = boot().await;
    let cwd = "/srv/repos/thing";
    let context = json!({ "ticket": "AB-1", "nested": { "n": 3 } });
    let gate = json!({ "steps": [{ "name": "accept", "cmd": "true" }] });
    let body = format!(
        "# Plan\n\nintro\n\n{}{}",
        task_fence(json!({
            "key": "setup",
            "goal": "set up",
            "kind": "codex",
            "no_gate_reason": "nothing to check yet",
        })),
        task_fence(json!({
            "key": "live",
            "goal": "do the thing",
            "kind": "codex",
            "refs": [foreign_block_ref()],
            "cwd": cwd,
            "context": context,
            "depends_on": ["setup"],
            "gate": gate,
        })),
    );
    let (status, created) = send(
        boot.app.clone(),
        "POST",
        "/api/track-recipes",
        Some("user"),
        Some(json!({ "title": "mine", "body": body })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={created}");

    let stored = created["body"].as_str().expect("body");
    let tasks = task_payloads(stored);
    assert_eq!(tasks.len(), 2, "tasks={tasks:?}");
    let live = &tasks[1];
    assert_eq!(live["key"], json!("live"));

    assert!(
        live.get("refs").is_none(),
        "`refs` must be removed, not emptied: {live:?}"
    );
    assert!(
        !stored.contains(FOREIGN_TRACK),
        "no trace of the foreign reference may remain in the body: {stored}"
    );

    assert_eq!(live["cwd"], json!(cwd), "cwd is the author's to choose");
    assert_eq!(live["context"], context, "context must survive whole");
    assert_eq!(live["depends_on"], json!(["setup"]));
    assert_eq!(live["gate"], gate);
}

#[tokio::test]
async fn a_non_task_fence_is_stored_in_canonical_form_with_its_payload_intact() {
    let boot = boot().await;
    let payload = json!({ "src": "/apps/x", "title": "X", "height": 400 });
    // Deliberately compact and in non-sorted key order: legal input that
    // `parse_fence` accepts and `render_fence` would never emit.
    let compact = "```neige-block app\n{\"title\":\"X\",\"src\":\"/apps/x\",\"height\":400}\n```\n";
    assert_ne!(
        compact,
        calm_types::report_blocks::render_fence("app", &payload),
        "the input must not already be canonical, or this test proves nothing"
    );
    let body = format!("# Plan\n\nintro\n\n{compact}");

    let (status, created) = send(
        boot.app.clone(),
        "POST",
        "/api/track-recipes",
        Some("user"),
        Some(json!({ "title": "mine", "body": body })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={created}");
    let stored = created["body"].as_str().expect("body");

    assert_eq!(
        fences(stored),
        vec![("app".to_string(), payload.clone())],
        "the payload must survive unchanged: {stored}"
    );
    assert!(
        stored.contains(&calm_types::report_blocks::render_fence("app", &payload)),
        "the fence must be stored in canonical form: {stored:?}"
    );
    assert!(
        stored.contains("intro"),
        "prose must survive verbatim: {stored}"
    );
}

/// Read with a real CommonMark parser: `foo` followed by `---` is a Setext H2, which no substring scan sees.
fn headings(body: &str) -> Vec<(u32, String)> {
    use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
    let mut out = Vec::new();
    let mut current: Option<(u32, String)> = None;
    for event in Parser::new(body) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let level = match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                };
                current = Some((level, String::new()));
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some((_, buffer)) = current.as_mut() {
                    buffer.push_str(&text);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(heading) = current.take() {
                    out.push(heading);
                }
            }
            _ => {}
        }
    }
    out
}

#[tokio::test]
async fn dropping_a_tombstone_does_not_splice_its_prose_neighbours() {
    let boot = boot().await;
    let body = format!(
        "# Plan\n\nfoo\n{}---\n\nbar\n",
        task_fence(json!({
            "key": "retired",
            "tombstone": { "reason": null },
            "declared_by": "user",
            "tombstoned_by": "user",
        })),
    );
    // Precondition: in the input, `foo` is a paragraph and the only heading
    // is `# Plan`. If this ever stops holding the test below proves nothing.
    assert_eq!(
        headings(&body),
        vec![(1, "Plan".to_string())],
        "input must start with exactly one heading: {body}"
    );

    let (status, created) = send(
        boot.app.clone(),
        "POST",
        "/api/track-recipes",
        Some("user"),
        Some(json!({ "title": "mine", "body": body })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={created}");
    let stored = created["body"].as_str().expect("body");

    assert!(
        task_payloads(stored).is_empty(),
        "the tombstone must still be dropped: {stored}"
    );
    assert_eq!(
        headings(stored),
        vec![(1, "Plan".to_string())],
        "dropping the task must not re-parse the prose around it: {stored:?}"
    );
}

#[tokio::test]
async fn normalization_is_byte_identical_the_second_time() {
    let boot = boot().await;
    let body = format!(
        "# Plan\n\nfoo\n{}---\n\nbar\n\n```neige-block app\n{{\"title\":\"X\",\"src\":\"/apps/x\"}}\n```\n\n{}{}end\n",
        task_fence(json!({
            "key": "retired",
            "tombstone": { "reason": null },
            "declared_by": "user",
        })),
        task_fence(json!({
            "key": "live",
            "goal": "do the thing",
            "kind": "codex",
            "refs": [foreign_block_ref()],
            "cwd": "/srv/repos/thing",
            "declared_by": "user",
            "ready": true,
            "released_by_user": true,
        })),
        task_fence(json!({
            "key": "also-retired",
            "tombstone": { "reason": "dropped" },
            "declared_by": "user",
        })),
    );
    let (status, created) = send(
        boot.app.clone(),
        "POST",
        "/api/track-recipes",
        Some("user"),
        Some(json!({ "title": "mine", "body": body })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={created}");
    let id = created["id"].as_str().unwrap().to_string();
    let first = created["body"].as_str().expect("body").to_string();

    let (status, updated) = send(
        boot.app.clone(),
        "PUT",
        &format!("/api/track-recipes/{id}"),
        Some("user"),
        Some(json!({ "title": "mine", "body": first, "if_revision": 1 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={updated}");
    let second = updated["body"].as_str().expect("body");
    assert_eq!(
        second, first,
        "re-storing a stored body must be the identity"
    );

    // The shape of the fixed point, which byte-identity cannot pin on its
    // own: `refs: []` would be just as stable as an absent key.
    for task in task_payloads(second) {
        assert!(
            task.get("refs").is_none(),
            "a stored recipe must carry no `refs` key, emptied or otherwise: {task:?}"
        );
    }
    assert!(
        second.contains("/srv/repos/thing"),
        "`cwd` is not dropped and must survive both passes: {second}"
    );
}

/// The shared block-endpoint 403 sends callers to the MCP `calm.report.*` tools, which do not write recipes.
#[tokio::test]
async fn the_recipe_403_explains_recipes_not_report_blocks() {
    let boot = boot().await;
    let (status, error) = send(
        boot.app.clone(),
        "POST",
        "/api/track-recipes",
        Some("ai:claude"),
        Some(json!({ "title": "theirs", "body": "# Plan\n" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={error}");
    let message = error["error"].as_str().expect("error message");
    assert!(
        message.contains("recipe"),
        "must name what was refused: {message}"
    );
    assert!(
        !message.contains("calm.report."),
        "must not redirect to a tool that cannot write recipes: {message}"
    );
    assert!(
        message.contains("ai:claude"),
        "must name the rejected actor: {message}"
    );
}

#[tokio::test]
async fn update_normalizes_too() {
    let boot = boot().await;
    let (_, created) = send(
        boot.app.clone(),
        "POST",
        "/api/track-recipes",
        Some("user"),
        Some(json!({ "title": "mine", "body": "# Plan\n\nintro\n" })),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    let dirty = task_fence(json!({
        "key": "k",
        "goal": "g",
        "kind": "codex",
        "declared_by": "user",
        "ready": true,
        "released_by_user": true,
    }));
    let (status, updated) = send(
        boot.app.clone(),
        "PUT",
        &format!("/api/track-recipes/{id}"),
        Some("user"),
        Some(json!({ "title": "mine", "body": dirty, "if_revision": 1 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={updated}");
    let tasks = task_payloads(updated["body"].as_str().unwrap());
    assert_eq!(tasks[0]["declared_by"], json!("spec"));
    assert_eq!(tasks[0]["ready"], json!(false));
    assert!(tasks[0].get("released_by_user").is_none());
    assert_eq!(updated["revision"], json!(2), "revision must bump");
}

#[tokio::test]
async fn a_recipe_may_have_zero_tasks() {
    let boot = boot().await;
    let (status, created) = send(
        boot.app.clone(),
        "POST",
        "/api/track-recipes",
        Some("user"),
        Some(json!({ "title": "empty", "body": "# Plan\n\njust prose\n" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={created}");
    assert!(task_payloads(created["body"].as_str().unwrap()).is_empty());
}

#[tokio::test]
async fn a_schema_violating_task_payload_is_a_400() {
    let boot = boot().await;
    // `key` present but `goal` missing on a live task.
    let body = task_fence(json!({ "key": "k", "ready": true, "declared_by": "spec" }));
    let (status, error) = send(
        boot.app.clone(),
        "POST",
        "/api/track-recipes",
        Some("user"),
        Some(json!({ "title": "bad", "body": body })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={error}");
    assert_eq!(error["code"], json!("bad_request"));
}

#[tokio::test]
async fn an_empty_title_is_refused() {
    let boot = boot().await;
    let (status, _) = send(
        boot.app.clone(),
        "POST",
        "/api/track-recipes",
        Some("user"),
        Some(json!({ "title": "   ", "body": "# Plan\n" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// `ai:claude`, deliberately: `Actor::to_actor_id` folds every other `ai:*` into `ActorId::User`, so `ai:codex` would prove nothing.
#[tokio::test]
async fn a_declared_agent_actor_may_not_write_recipes() {
    let boot = boot().await;
    let (_, created) = send(
        boot.app.clone(),
        "POST",
        "/api/track-recipes",
        Some("user"),
        Some(json!({ "title": "mine", "body": "# Plan\n" })),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();

    for (method, uri, body) in [
        (
            "POST",
            "/api/track-recipes".to_string(),
            Some(json!({ "title": "theirs", "body": "# Plan\n" })),
        ),
        (
            "PUT",
            format!("/api/track-recipes/{id}"),
            Some(json!({ "title": "theirs", "body": "# Plan\n", "if_revision": 1 })),
        ),
        ("DELETE", format!("/api/track-recipes/{id}"), None),
    ] {
        let (status, error) = send(
            boot.app.clone(),
            method,
            &uri,
            Some("ai:claude"),
            body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {error}");
    }

    let (status, still) = send(
        boot.app.clone(),
        "GET",
        &format!("/api/track-recipes/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(still["title"], json!("mine"));
    assert_eq!(still["revision"], json!(1));
}

#[tokio::test]
async fn a_user_actor_may_write_recipes() {
    let boot = boot().await;
    for actor in [Some("user"), None] {
        let (status, created) = send(
            boot.app.clone(),
            "POST",
            "/api/track-recipes",
            actor,
            Some(json!({ "title": "mine", "body": "# Plan\n" })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "actor={actor:?}: {created}");
    }
}

#[tokio::test]
async fn a_stale_revision_is_a_409_that_writes_nothing() {
    let boot = boot().await;
    let (_, created) = send(
        boot.app.clone(),
        "POST",
        "/api/track-recipes",
        Some("user"),
        Some(json!({ "title": "mine", "body": "# Plan\n" })),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();

    let (first, _) = send(
        boot.app.clone(),
        "PUT",
        &format!("/api/track-recipes/{id}"),
        Some("user"),
        Some(json!({ "title": "first", "body": "# Plan\n\nfirst\n", "if_revision": 1 })),
    )
    .await;
    assert_eq!(first, StatusCode::OK);

    let (second, error) = send(
        boot.app.clone(),
        "PUT",
        &format!("/api/track-recipes/{id}"),
        Some("user"),
        Some(json!({ "title": "second", "body": "# Plan\n\nsecond\n", "if_revision": 1 })),
    )
    .await;
    assert_eq!(second, StatusCode::CONFLICT, "body={error}");

    let (_, current) = send(
        boot.app.clone(),
        "GET",
        &format!("/api/track-recipes/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(current["title"], json!("first"), "loser must not have won");
    assert!(current["body"].as_str().unwrap().contains("first"));
    assert_eq!(current["revision"], json!(2), "exactly one bump");
}

#[tokio::test]
async fn unknown_recipe_is_404_on_every_verb() {
    let boot = boot().await;
    for (method, body) in [
        ("GET", None),
        (
            "PUT",
            Some(json!({ "title": "x", "body": "# Plan\n", "if_revision": 1 })),
        ),
        ("DELETE", None),
    ] {
        let (status, _) = send(
            boot.app.clone(),
            method,
            "/api/track-recipes/does-not-exist",
            Some("user"),
            body,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method}");
    }
}

#[tokio::test]
async fn delete_removes_it_from_the_list() {
    let boot = boot().await;
    let (_, created) = send(
        boot.app.clone(),
        "POST",
        "/api/track-recipes",
        Some("user"),
        Some(json!({ "title": "mine", "body": "# Plan\n" })),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    let (status, _) = send(
        boot.app.clone(),
        "DELETE",
        &format!("/api/track-recipes/{id}"),
        Some("user"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, list) = send(boot.app.clone(), "GET", "/api/track-recipes", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(list.as_array().unwrap().is_empty(), "list={list}");
}

/// Keys out of declaration order and an explicit default, so a canonical stored line proves a rewrite.
const NON_CANONICAL_HEADER: &str = "<!-- neige:contract {\"sections\":[{\"omit_if_empty\":false,\"h1\":\"概要\"}],\"version\":1} -->";

fn one_section_header() -> calm_types::report_contract::ContractHeader {
    calm_types::report_contract::ContractHeader {
        version: 1,
        sections: vec![calm_types::report_contract::ContractSection {
            h1: "概要".into(),
            omit_if_empty: false,
        }],
    }
}

fn first_line(body: &str) -> &str {
    body.split('\n').next().unwrap_or_default()
}

/// `+++` opens a template file's TOML front matter; a body starting with it was pasted whole.
#[tokio::test]
async fn a_body_that_starts_with_front_matter_is_a_400_on_both_verbs() {
    let boot = boot().await;
    let front_matter = "+++\nid = \"pasted\"\n+++\n# Plan\n";
    let (status, error) = send(
        boot.app.clone(),
        "POST",
        "/api/track-recipes",
        Some("user"),
        Some(json!({ "title": "pasted", "body": front_matter })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={error}");
    assert!(
        error["error"].as_str().unwrap_or_default().contains("+++"),
        "the message names the prefix: {error}"
    );

    let (_, created) = send(
        boot.app.clone(),
        "POST",
        "/api/track-recipes",
        Some("user"),
        Some(json!({ "title": "fine", "body": "# Plan\n" })),
    )
    .await;
    let id = created["id"].as_str().unwrap();
    let (status, error) = send(
        boot.app.clone(),
        "PUT",
        &format!("/api/track-recipes/{id}"),
        Some("user"),
        Some(json!({ "title": "fine", "body": front_matter, "if_revision": created["revision"] })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={error}");
    assert!(
        error["error"].as_str().unwrap_or_default().contains("+++"),
        "the message names the prefix: {error}"
    );
    let (_, unchanged) = send(
        boot.app.clone(),
        "GET",
        &format!("/api/track-recipes/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(
        unchanged["body"],
        json!("# Plan\n"),
        "the refused PUT wrote nothing"
    );
}

#[tokio::test]
async fn a_non_canonical_header_is_stored_and_read_back_canonical() {
    use calm_types::report_contract::canonical_line;

    let boot = boot().await;
    let (status, created) = send(
        boot.app.clone(),
        "POST",
        "/api/track-recipes",
        Some("user"),
        Some(json!({
            "title": "headed",
            "body": format!("{NON_CANONICAL_HEADER}\n\n# 概要\n\nprose\n")
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={created}");
    let canonical = canonical_line(&one_section_header());
    assert_ne!(
        NON_CANONICAL_HEADER, canonical,
        "fixture must be non-canonical"
    );
    assert_eq!(first_line(created["body"].as_str().unwrap()), canonical);

    let id = created["id"].as_str().unwrap();
    let (status, fetched) = send(
        boot.app.clone(),
        "GET",
        &format!("/api/track-recipes/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first_line(fetched["body"].as_str().unwrap()), canonical);
    assert_eq!(
        &fetched["body"].as_str().unwrap()[canonical.len()..],
        "\n\n# 概要\n\nprose\n",
        "only line 1 was rewritten"
    );
}

#[tokio::test]
async fn a_header_off_line_1_is_a_400() {
    let boot = boot().await;
    let (status, error) = send(
        boot.app.clone(),
        "POST",
        "/api/track-recipes",
        Some("user"),
        Some(json!({
            "title": "late header",
            "body": format!("# Plan\n\n{NON_CANONICAL_HEADER}\n")
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={error}");
    assert_eq!(error["code"], json!("bad_request"));
    assert!(
        error["error"]
            .as_str()
            .unwrap_or_default()
            .contains("first line"),
        "the header's own Misplaced message: {error}"
    );
}

#[tokio::test]
async fn update_stores_a_non_canonical_header_canonical() {
    use calm_types::report_contract::canonical_line;

    let boot = boot().await;
    let (_, created) = send(
        boot.app.clone(),
        "POST",
        "/api/track-recipes",
        Some("user"),
        Some(json!({ "title": "mine", "body": "# Plan\n" })),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    let (status, updated) = send(
        boot.app.clone(),
        "PUT",
        &format!("/api/track-recipes/{id}"),
        Some("user"),
        Some(json!({
            "title": "mine",
            "body": format!("{NON_CANONICAL_HEADER}\n\n# 概要\n\nprose\n"),
            "if_revision": created["revision"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={updated}");
    let canonical = canonical_line(&one_section_header());
    assert_eq!(first_line(updated["body"].as_str().unwrap()), canonical);

    let (status, fetched) = send(
        boot.app.clone(),
        "GET",
        &format!("/api/track-recipes/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first_line(fetched["body"].as_str().unwrap()), canonical);
    assert_eq!(
        &fetched["body"].as_str().unwrap()[canonical.len()..],
        "\n\n# 概要\n\nprose\n",
        "only line 1 was rewritten"
    );
}

#[tokio::test]
async fn update_with_a_header_off_line_1_is_a_400_that_writes_nothing() {
    let boot = boot().await;
    let (_, created) = send(
        boot.app.clone(),
        "POST",
        "/api/track-recipes",
        Some("user"),
        Some(json!({ "title": "mine", "body": "# Plan\n" })),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    let (status, error) = send(
        boot.app.clone(),
        "PUT",
        &format!("/api/track-recipes/{id}"),
        Some("user"),
        Some(json!({
            "title": "mine",
            "body": format!("# Plan\n\n{NON_CANONICAL_HEADER}\n"),
            "if_revision": created["revision"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={error}");
    assert_eq!(error["code"], json!("bad_request"));
    assert!(
        error["error"]
            .as_str()
            .unwrap_or_default()
            .contains("first line"),
        "the header's own Misplaced message: {error}"
    );
    let (_, unchanged) = send(
        boot.app.clone(),
        "GET",
        &format!("/api/track-recipes/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(unchanged["body"], json!("# Plan\n"));
    assert_eq!(
        unchanged["revision"], created["revision"],
        "the refused PUT wrote nothing"
    );
}
