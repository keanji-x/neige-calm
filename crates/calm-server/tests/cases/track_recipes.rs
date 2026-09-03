//! #1292 S1 — `/api/track-recipes`, user-defined starting points.
//!
//! What these pin, and why each needs pinning:
//!
//!   * **Normalization at the write boundary.** A recipe must not carry one
//!     track's authority into every track made from it. The privilege fields
//!     go through the same function fork uses; tombstones are dropped, which
//!     is the one place recipes deliberately differ from fork.
//!   * **The actor gate, with `ai:claude` as the negative.** `ai:codex`
//!     would prove nothing: `Actor::to_actor_id` folds every *other* `ai:*`
//!     into `ActorId::User`, so a gate written against the typed value would
//!     pass `ai:claude` and still look correct in a test that only tried
//!     `ai:codex`.
//!   * **409 writes nothing.** A conflict that still committed would be
//!     indistinguishable from a correct refusal by status code alone.

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

/// Every parsed fence in a body, as `(kind, payload)`, in order.
fn fences(body: &str) -> Vec<(String, Value)> {
    calm_types::report_blocks::split_body(body)
        .iter()
        .filter_map(|slice| calm_types::report_blocks::parse_fence(&slice.raw))
        .map(|fence| (fence.kind, fence.payload))
        .collect()
}

// ---------------------------------------------------------------------------
// Normalization
// ---------------------------------------------------------------------------

/// All four normalizations in one body, asserted field by field.
///
/// `released_by_user` must be **absent**, not `false`: the two are identical
/// to every reader but not to `track_report_edit_guard`, which compares the
/// raw `Option<&Value>`. Absent is the shape a fresh declaration has.
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

/// A non-task fence is re-rendered into canonical form, with its payload
/// untouched.
///
/// Why this matters and a semantic check would not: instantiation re-renders
/// every fence it can parse, so a recipe holding a compact `app` payload
/// would produce a track whose bytes differ from the ones the picker showed —
/// same meaning, different document, and "the recipe and its instantiation
/// are the same bytes" would be false. Canonicalising at the write boundary
/// is what makes that re-render the identity.
///
/// The `Value` assertion is the other half: normalization may change how the
/// payload is spelled and nothing else. A non-task fence carries no task
/// authority, so nothing in it is the recipe's business to edit.
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

/// Every Markdown heading in `body`, as `(level, text)`, read with a real
/// CommonMark parser.
///
/// Deliberately not a substring scan: the bug this pins is that dropping a
/// tombstone *creates* a heading out of prose that contains no heading
/// marker at all (`foo\n---\n` is a Setext H2), so only a parser can see it.
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

/// Dropping a tombstone must not change how its *neighbours* parse.
///
/// The shape is specific and adversarial: the prose before the fence ends on
/// a non-blank line (`foo`), the prose after it opens with `---`. Spliced
/// naively those two become `foo\n---\n` — a Setext H2 titled "foo" — so
/// "delete one task" would silently promote a paragraph to a heading and
/// swallow the thematic break. The oracle is the whole heading list, before
/// and after: nothing about the surrounding prose may move.
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

/// Normalization is a fixed point: storing what a store returned changes
/// nothing, byte for byte.
///
/// Without this, the fix above could pay for its paragraph break by growing
/// the body a blank line on every save — the same content, re-saved from the
/// editor a few times, would drift.
///
/// The body carries a **non-task** fence too, in compact spelling: the second
/// pass is only the identity if the first pass already canonicalised it.
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
}

/// The 403 a refused recipe write returns must talk about **recipes**.
///
/// The decision is the block endpoints' shared helper, but their sentence
/// sends the caller to the MCP `calm.report.*` tools — advice that is simply
/// wrong here: no MCP tool writes recipes.
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

/// The same normalization on the update path. Without this, a recipe could
/// be created clean and then edited dirty.
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

/// A recipe with no tasks at all is legal end to end — the body fence
/// validator accepts it, the declaration projection is empty, and task
/// projection takes an empty slice. Pinned so nobody later "helpfully" adds
/// a minimum-one-task rule.
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

/// A body whose fence is well-formed but whose payload violates the task
/// schema is a **400**, not a 500: this input came from the caller, unlike
/// `prepare_template_report`'s Rust constants.
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

// ---------------------------------------------------------------------------
// Actor gate
// ---------------------------------------------------------------------------

/// `ai:claude`, deliberately — see this module's header. Every write verb is
/// covered because a gate is only as good as its least-guarded entry point.
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

    // …and the recipe is untouched.
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

/// The positive half. Without it the gate could be "reject everything" and
/// every negative test above would still pass.
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

// ---------------------------------------------------------------------------
// Optimistic locking
// ---------------------------------------------------------------------------

/// Two writers holding the same `revision`: the second is refused **and
/// writes nothing**. A conflict that still committed would pass a
/// status-code-only assertion.
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
