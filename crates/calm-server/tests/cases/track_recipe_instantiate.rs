//! Creating a track from a user-defined recipe, and the provenance it records.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::NewArea;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use calm_server::track_report::TrackReportPayload;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::common;
use crate::support::git_helpers::attached_repo_fixture;

struct Boot {
    app: axum::Router,
    area_id: String,
    repo: Arc<dyn Repo>,
    /// Un-erased so the provenance tests can probe the cross-column CHECK directly.
    sqlx_repo: Arc<SqlxRepo>,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let sqlx_repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let area = repo
        .area_create(NewArea {
            name: "recipe-instantiate".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();
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
            std::env::temp_dir().join("calm-plugins-data-1292-s2"),
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
    Boot {
        app,
        area_id: area.id.to_string(),
        repo,
        sqlx_repo,
        _tmp: tmp,
    }
}

fn theme() -> Value {
    json!({"fg": [216, 219, 226], "bg": [15, 20, 24]})
}

async fn send(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("X-Calm-Actor", "user");
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

pub(crate) fn task_fence(payload: Value) -> String {
    format!(
        "```neige-block task\n{}\n```\n",
        serde_json::to_string_pretty(&payload).unwrap()
    )
}

/// A recipe body with two tasks, one depending on the other.
pub(crate) fn two_task_body() -> String {
    format!(
        "# Plan\n\nSet the thing up, then check it.\n\n{}{}",
        task_fence(json!({
            "key": "setup",
            "goal": "set the thing up",
            "kind": "codex",
            "acceptance": "it is set up",
        })),
        task_fence(json!({
            "key": "verify",
            "goal": "check it",
            "kind": "codex",
            "depends_on": ["setup"],
        })),
    )
}

async fn create_recipe(app: axum::Router, title: &str, body: &str) -> Value {
    let (status, created) = send(
        app,
        "POST",
        "/api/track-recipes",
        Some(json!({ "title": title, "body": body })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "recipe create: {created}");
    created
}

fn create_track_body(area_id: &str, title: &str, extra: Value) -> Value {
    let mut body = json!({
        "planner_provider": "codex",
        "area_id": area_id,
        "title": title,
        "cwd": attached_repo_fixture(&format!("1292-s2-{title}")),
        "attach_folder": true,
        "theme": theme(),
    });
    if let (Value::Object(extra), Value::Object(obj)) = (extra, &mut body) {
        obj.extend(extra);
    }
    body
}

fn report_payload(detail: &Value) -> TrackReportPayload {
    let card = detail["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["kind"] == "track-report")
        .expect("track-report card");
    serde_json::from_value(card["payload"].clone()).expect("report payload")
}

fn template_context(detail: &Value) -> &Value {
    &detail["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["payload"]["planner_harness"] == true)
        .unwrap()["payload"]["template_context"]
}

fn task_blocks(payload: &TrackReportPayload) -> Vec<&Value> {
    payload
        .blocks
        .as_ref()
        .into_iter()
        .flatten()
        .filter(|block| block.kind == "task")
        .map(|block| &block.payload)
        .collect()
}

async fn track_detail(app: axum::Router, track_id: &str) -> Value {
    let (status, detail) = send(app, "GET", &format!("/api/tracks/{track_id}"), None).await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    detail
}

#[tokio::test]
async fn a_recipe_becomes_the_new_tracks_report() {
    let boot = boot().await;
    let recipe = create_recipe(boot.app.clone(), "my flow", &two_task_body()).await;
    let recipe_id = recipe["id"].as_str().unwrap().to_string();

    let (status, created) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "from-recipe",
            json!({ "recipe_id": recipe_id }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "track create: {created}");
    let track_id = created["id"].as_str().unwrap().to_string();

    let detail = track_detail(boot.app.clone(), &track_id).await;
    let payload = report_payload(&detail);
    assert_eq!(payload.summary, "my flow", "title becomes the summary");

    assert!(
        task_blocks(&payload).is_empty(),
        "saved steps are reference material only"
    );
    assert_eq!(template_context(&detail)["title"], recipe["title"]);
    assert_eq!(template_context(&detail)["body"], recipe["body"]);

    assert!(
        payload.body.contains("Set the thing up, then check it."),
        "prose survives: {}",
        payload.body
    );
}

#[tokio::test]
async fn a_recipe_that_carried_refs_instantiates_with_no_reference() {
    let boot = boot().await;
    let body = format!(
        "# Plan\n\nSet the thing up.\n\n{}",
        task_fence(json!({
            "key": "setup",
            "goal": "set the thing up",
            "kind": "codex",
            "cwd": "/srv/repos/thing",
            // Keeps the reference the only thing that could make this task unschedulable.
            "no_gate_reason": "nothing to check yet",
            "refs": [calm_types::report_links::format_track_destination(
                "some-other-track",
                Some("b_1f3a"),
            )],
        })),
    );
    let recipe = create_recipe(boot.app.clone(), "refs flow", &body).await;

    let (status, created) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "from-refs-recipe",
            json!({ "recipe_id": recipe["id"].as_str().unwrap() }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "track create: {created}");
    let track_id = created["id"].as_str().unwrap().to_string();

    let detail = track_detail(boot.app.clone(), &track_id).await;
    let payload = report_payload(&detail);
    assert!(task_blocks(&payload).is_empty());
    let context = template_context(&detail);
    assert_eq!(context["body"], recipe["body"]);
    assert!(
        !context["body"]
            .as_str()
            .unwrap()
            .contains("some-other-track"),
        "a saved recipe must not import source-track references into the startup prompt"
    );
    assert!(
        context["body"]
            .as_str()
            .unwrap()
            .contains("/srv/repos/thing")
    );

    let blocks = payload.blocks.as_deref().expect("report blocks");
    let verdicts = boot
        .repo
        .task_diagnostics(
            &track_id,
            blocks,
            calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
        )
        .await
        .expect("task_diagnostics");
    let blocking: Vec<_> = verdicts
        .iter()
        .flat_map(|verdict| &verdict.diagnostics)
        .filter(|diagnostic| diagnostic.path == "refs")
        .collect();
    assert!(
        blocking.is_empty(),
        "a recipe-borne reference left the task unschedulable: {blocking:#?}"
    );
    // Not `schedulable`: recipe normalization sets `ready: false`, so it is always false here.
    assert!(
        verdicts
            .iter()
            .all(|verdict| verdict.diagnostics.is_empty()),
        "verdicts={verdicts:#?}"
    );
}

#[tokio::test]
async fn a_recipe_created_track_has_no_template_id() {
    let boot = boot().await;
    let recipe = create_recipe(boot.app.clone(), "my flow", &two_task_body()).await;
    let (status, created) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "no-template-id",
            json!({ "recipe_id": recipe["id"].as_str().unwrap() }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let track_id = created["id"].as_str().unwrap();
    let track = boot
        .repo
        .track_get(track_id)
        .await
        .expect("track_get")
        .expect("track exists");
    assert_eq!(
        track.template_id, None,
        "a recipe is not a plugin-bindable template id"
    );
}

#[tokio::test]
async fn recipe_and_instantiated_track_are_independent() {
    let boot = boot().await;
    let recipe = create_recipe(boot.app.clone(), "v1", &two_task_body()).await;
    let recipe_id = recipe["id"].as_str().unwrap().to_string();

    let (_, created) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "snapshot",
            json!({ "recipe_id": recipe_id }),
        )),
    )
    .await;
    let track_id = created["id"].as_str().unwrap().to_string();

    let (status, updated) = send(
        boot.app.clone(),
        "PUT",
        &format!("/api/track-recipes/{recipe_id}"),
        Some(json!({
            "title": "v2",
            "body": format!("# Plan\n\nrewritten\n\n{}", task_fence(json!({
                "key": "different",
                "goal": "something else",
                "kind": "codex",
            }))),
            "if_revision": 1,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");

    // The existing track is untouched.
    let detail = track_detail(boot.app.clone(), &track_id).await;
    let payload = report_payload(&detail);
    assert_eq!(payload.summary, "v1", "the track kept its snapshot");
    assert!(task_blocks(&payload).is_empty());
    assert_eq!(template_context(&detail)["body"], recipe["body"]);

    let (_, second) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "after-edit",
            json!({ "recipe_id": recipe_id }),
        )),
    )
    .await;
    let second_detail = track_detail(boot.app.clone(), second["id"].as_str().unwrap()).await;
    let second_payload = report_payload(&second_detail);
    assert_eq!(second_payload.summary, "v2");
    assert!(task_blocks(&second_payload).is_empty());
    assert_eq!(template_context(&second_detail)["body"], updated["body"]);
}

#[tokio::test]
async fn deleting_a_recipe_does_not_disturb_tracks_made_from_it() {
    let boot = boot().await;
    let recipe = create_recipe(boot.app.clone(), "doomed", &two_task_body()).await;
    let recipe_id = recipe["id"].as_str().unwrap().to_string();
    let (_, created) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "survivor",
            json!({ "recipe_id": recipe_id }),
        )),
    )
    .await;
    let track_id = created["id"].as_str().unwrap().to_string();

    let (status, _) = send(
        boot.app.clone(),
        "DELETE",
        &format!("/api/track-recipes/{recipe_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let detail = track_detail(boot.app.clone(), &track_id).await;
    let payload = report_payload(&detail);
    assert_eq!(payload.summary, "doomed");
    assert!(task_blocks(&payload).is_empty());
    assert_eq!(template_context(&detail)["body"], recipe["body"]);
}

#[tokio::test]
async fn an_unknown_recipe_id_is_a_400() {
    let boot = boot().await;
    let (status, error) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "ghost",
            json!({ "recipe_id": "does-not-exist" }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert!(
        error["error"]
            .as_str()
            .unwrap_or_default()
            .contains("does-not-exist"),
        "the message must name it: {error}"
    );
}

#[tokio::test]
async fn template_id_and_recipe_id_together_are_a_400() {
    let boot = boot().await;
    let recipe = create_recipe(boot.app.clone(), "mine", &two_task_body()).await;
    let (status, error) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "ambiguous",
            json!({
                "template_id": "small-change",
                "recipe_id": recipe["id"].as_str().unwrap(),
            }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["code"], json!("bad_request"), "{error}");
    let message = error["error"].as_str().unwrap_or("");
    for field in ["template_id", "recipe_id"] {
        assert!(
            message.contains(&format!("`{field}`")),
            "the 400 must name the fields that collided; got {error}"
        );
    }
    assert!(
        !message.contains("`fork_report_from`"),
        "the 400 must not name a field the caller did not send; got {error}"
    );
}

#[tokio::test]
async fn template_id_and_recipe_id_with_a_fork_source_are_still_a_400() {
    let boot = boot().await;
    let recipe = create_recipe(boot.app.clone(), "mine", &two_task_body()).await;

    // A real, forkable source track, so the refusal is for ambiguity and not a dangling id.
    let (_, source) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(&boot.area_id, "fork-source", json!({}))),
    )
    .await;
    let source_id = source["id"].as_str().unwrap().to_string();

    let (status, error) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "ambiguous-with-fork",
            json!({
                "template_id": "small-change",
                "recipe_id": recipe["id"].as_str().unwrap(),
                "fork_report_from": source_id,
            }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["code"], json!("bad_request"), "{error}");
    let message = error["error"].as_str().unwrap_or("");
    for field in ["template_id", "recipe_id", "fork_report_from"] {
        assert!(
            message.contains(&format!("`{field}`")),
            "the 400 must name every field that was sent; got {error}"
        );
    }
}

#[tokio::test]
async fn a_recipe_and_an_explicit_fork_source_are_a_400() {
    let boot = boot().await;
    let recipe = create_recipe(boot.app.clone(), "recipe-side", &two_task_body()).await;

    let (_, source) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(&boot.area_id, "fork-source", json!({}))),
    )
    .await;
    let source_id = source["id"].as_str().unwrap().to_string();

    // Control: each half alone is a legal create, so the refusal below is about
    // the combination and not about either id.
    for (leg, extra) in [
        ("recipe-only", json!({ "recipe_id": recipe["id"].clone() })),
        (
            "fork-only",
            json!({ "fork_report_from": source_id.clone() }),
        ),
    ] {
        let (status, created) = send(
            boot.app.clone(),
            "POST",
            "/api/tracks",
            Some(create_track_body(
                &boot.area_id,
                &format!("control-{leg}"),
                extra,
            )),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{leg}: {created}");
    }

    let (status, error) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "recipe-plus-fork",
            json!({
                "recipe_id": recipe["id"].as_str().unwrap(),
                "fork_report_from": source_id,
            }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["code"], json!("bad_request"), "{error}");
    let message = error["error"].as_str().unwrap_or("");
    assert!(
        message.contains("`recipe_id`") && message.contains("`fork_report_from`"),
        "the 400 must name both offending fields; got {error}"
    );
    assert!(
        !message.contains("`template_id`"),
        "it must name the fields that were actually sent; got {error}"
    );
}

#[tokio::test]
async fn a_zero_task_recipe_instantiates() {
    let boot = boot().await;
    let recipe = create_recipe(boot.app.clone(), "empty", "# Plan\n\njust prose\n").await;
    let (status, created) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "empty-track",
            json!({ "recipe_id": recipe["id"].as_str().unwrap() }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let payload =
        report_payload(&track_detail(boot.app.clone(), created["id"].as_str().unwrap()).await);
    assert!(task_blocks(&payload).is_empty());
    assert_eq!(payload.summary, "empty");
}

/// Reads back through a real SELECT so a column missing from `TRACK_SELECT_COLUMNS` fails here.
#[tokio::test]
async fn a_recipe_created_track_records_which_recipe_and_which_revision() {
    let boot = boot().await;
    let recipe = create_recipe(boot.app.clone(), "traceable", &two_task_body()).await;
    let recipe_id = recipe["id"].as_str().unwrap().to_string();
    assert_eq!(recipe["revision"], json!(1), "fresh recipe: {recipe}");

    let (status, created) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "traced",
            json!({ "recipe_id": recipe_id }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let track_id = created["id"].as_str().unwrap().to_string();

    let track = boot
        .repo
        .track_get(&track_id)
        .await
        .expect("track_get")
        .expect("track exists");
    assert_eq!(track.recipe_id.as_deref(), Some(recipe_id.as_str()));
    assert_eq!(track.recipe_revision, Some(1));
}

/// Reads the provenance back through `TRACK_SELECT_COLUMNS_W`, the detail query's column list.
#[tokio::test]
async fn the_track_detail_route_carries_the_provenance() {
    let boot = boot().await;
    let recipe = create_recipe(boot.app.clone(), "traceable-detail", &two_task_body()).await;
    let recipe_id = recipe["id"].as_str().unwrap().to_string();

    let (status, created) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "traced-detail",
            json!({ "recipe_id": recipe_id }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let detail = track_detail(boot.app.clone(), created["id"].as_str().unwrap()).await;
    assert_eq!(detail["track"]["recipe_id"], json!(recipe_id), "{detail}");
    assert_eq!(detail["track"]["recipe_revision"], json!(1), "{detail}");
}

#[tokio::test]
async fn a_track_not_made_from_a_recipe_has_no_provenance() {
    let boot = boot().await;
    let (status, created) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(&boot.area_id, "plain", json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let track = boot
        .repo
        .track_get(created["id"].as_str().unwrap())
        .await
        .expect("track_get")
        .expect("track exists");
    assert_eq!(track.recipe_id, None);
    assert_eq!(track.recipe_revision, None);
}

#[tokio::test]
async fn editing_the_recipe_leaves_an_existing_tracks_revision_alone() {
    let boot = boot().await;
    let recipe = create_recipe(boot.app.clone(), "v1", &two_task_body()).await;
    let recipe_id = recipe["id"].as_str().unwrap().to_string();

    let (_, before) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "before-edit-prov",
            json!({ "recipe_id": recipe_id }),
        )),
    )
    .await;
    let before_id = before["id"].as_str().unwrap().to_string();

    let (status, updated) = send(
        boot.app.clone(),
        "PUT",
        &format!("/api/track-recipes/{recipe_id}"),
        Some(json!({
            "title": "v2",
            "body": "# Plan\n\nrewritten\n",
            "if_revision": 1,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["revision"], json!(2), "the edit bumped it");

    let older = boot
        .repo
        .track_get(&before_id)
        .await
        .expect("track_get")
        .expect("track exists");
    assert_eq!(
        older.recipe_revision,
        Some(1),
        "the recorded revision names the version this track was built from"
    );
    assert_eq!(older.recipe_id.as_deref(), Some(recipe_id.as_str()));

    let (_, after) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "after-edit-prov",
            json!({ "recipe_id": recipe_id }),
        )),
    )
    .await;
    let newer = boot
        .repo
        .track_get(after["id"].as_str().unwrap())
        .await
        .expect("track_get")
        .expect("track exists");
    assert_eq!(newer.recipe_revision, Some(2));
}

#[tokio::test]
async fn deleting_the_recipe_leaves_the_provenance_readable() {
    let boot = boot().await;
    let recipe = create_recipe(boot.app.clone(), "doomed-prov", &two_task_body()).await;
    let recipe_id = recipe["id"].as_str().unwrap().to_string();

    let (_, created) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "survivor-prov",
            json!({ "recipe_id": recipe_id }),
        )),
    )
    .await;
    let track_id = created["id"].as_str().unwrap().to_string();

    let (status, _) = send(
        boot.app.clone(),
        "DELETE",
        &format!("/api/track-recipes/{recipe_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let track = boot
        .repo
        .track_get(&track_id)
        .await
        .expect("track_get")
        .expect("track still readable");
    assert_eq!(track.recipe_id.as_deref(), Some(recipe_id.as_str()));
    assert_eq!(track.recipe_revision, Some(1));

    let detail = track_detail(boot.app.clone(), &track_id).await;
    assert_eq!(detail["track"]["recipe_id"], json!(recipe_id), "{detail}");
}

/// Written straight at the database: no repo writer can produce half a provenance.
#[tokio::test]
async fn the_database_refuses_half_a_provenance() {
    let boot = boot().await;
    let recipe = create_recipe(boot.app.clone(), "half", &two_task_body()).await;
    let (_, created) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "half-prov",
            json!({ "recipe_id": recipe["id"].as_str().unwrap() }),
        )),
    )
    .await;
    let track_id = created["id"].as_str().unwrap().to_string();
    let pool = boot.sqlx_repo.pool();
    const REFUSED_BY: &str = "CHECK constraint failed: track_recipe_origin_is_whole";

    let error = sqlx::query("UPDATE tracks SET recipe_revision = NULL WHERE id = ?1")
        .bind(&track_id)
        .execute(pool)
        .await
        .expect_err("a recipe id with no revision must be refused");
    assert!(
        error.to_string().contains(REFUSED_BY),
        "expected {REFUSED_BY} to be what refused it, got: {error}"
    );

    let error = sqlx::query("UPDATE tracks SET recipe_id = NULL WHERE id = ?1")
        .bind(&track_id)
        .execute(pool)
        .await
        .expect_err("a revision with no recipe id must be refused");
    assert!(
        error.to_string().contains(REFUSED_BY),
        "expected {REFUSED_BY} to be what refused it, got: {error}"
    );

    let error = sqlx::query(
        "INSERT INTO tracks \
           (id, area_id, title, sort, created_at, updated_at, recipe_id, recipe_revision) \
         SELECT 'half-inserted', area_id, title, sort + 1.0, created_at, updated_at, \
                'some-recipe', NULL \
         FROM tracks WHERE id = ?1",
    )
    .bind(&track_id)
    .execute(pool)
    .await
    .expect_err("an INSERT carrying half a provenance must be refused too");
    assert!(
        error.to_string().contains(REFUSED_BY),
        "expected {REFUSED_BY} to be what refused it, got: {error}"
    );

    sqlx::query("UPDATE tracks SET recipe_id = NULL, recipe_revision = NULL WHERE id = ?1")
        .bind(&track_id)
        .execute(pool)
        .await
        .expect("clearing both at once is a state the system has a reading for");
}

#[tokio::test]
async fn a_fork_of_a_recipe_born_track_has_no_provenance() {
    let boot = boot().await;
    let recipe = create_recipe(boot.app.clone(), "forkable", &two_task_body()).await;
    let recipe_id = recipe["id"].as_str().unwrap().to_string();

    let (status, source) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "recipe-born",
            json!({ "recipe_id": recipe_id }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{source}");
    let source_id = source["id"].as_str().unwrap().to_string();

    let source_track = boot
        .repo
        .track_get(&source_id)
        .await
        .expect("track_get")
        .expect("source exists");
    assert_eq!(source_track.recipe_id.as_deref(), Some(recipe_id.as_str()));

    let (status, forked) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "fork-of-recipe-born",
            json!({ "fork_report_from": source_id }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{forked}");
    let fork_id = forked["id"].as_str().unwrap().to_string();

    // The fork did receive the recipe's content, one hop removed.
    let detail = track_detail(boot.app.clone(), &fork_id).await;
    let payload = report_payload(&detail);
    assert_eq!(payload.summary, "forkable");
    assert!(task_blocks(&payload).is_empty());
    assert!(payload.body.contains("Set the thing up, then check it."));
    assert!(
        template_context(&detail).is_null(),
        "a report fork does not inherit startup instructions"
    );

    let fork = boot
        .repo
        .track_get(&fork_id)
        .await
        .expect("track_get")
        .expect("fork exists");
    assert_eq!(
        fork.recipe_id, None,
        "a fork was instantiated from a track, not from a recipe"
    );
    assert_eq!(fork.recipe_revision, None);
}

/// The write boundary refuses a `+++` body, so the row is written through the repo directly.
#[tokio::test]
async fn a_stored_recipe_that_starts_with_front_matter_does_not_instantiate() {
    let boot = boot().await;
    let stored = boot
        .repo
        .track_recipe_create(calm_types::model::NewTrackRecipe {
            title: "pre-boundary".into(),
            body: "+++\nid = \"pasted\"\n+++\n# Plan\n".into(),
        })
        .await
        .expect("the repo itself does not validate bodies");
    let tracks_before = boot
        .repo
        .tracks_by_area(&boot.area_id)
        .await
        .expect("list tracks")
        .len();

    let (status, error) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "from-pasted",
            json!({ "recipe_id": stored.id }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["code"], json!("bad_request"));
    let message = error["error"].as_str().unwrap_or_default();
    assert!(
        message.contains("+++") && message.contains(&stored.id),
        "names the prefix and the recipe: {error}"
    );
    assert_eq!(
        boot.repo
            .tracks_by_area(&boot.area_id)
            .await
            .expect("list tracks")
            .len(),
        tracks_before,
        "no track was minted"
    );
}

/// Inserted through the repo so the header reaches the funnel untouched by any ingress.
#[tokio::test]
async fn a_stored_recipe_with_the_header_off_line_1_does_not_instantiate() {
    use calm_types::report_contract::canonical_line;
    use calm_types::track_report::work_brief_header;

    let boot = boot().await;
    let stored = boot
        .repo
        .track_recipe_create(calm_types::model::NewTrackRecipe {
            title: "late header".into(),
            body: format!("# Plan\n{}\n", canonical_line(&work_brief_header())),
        })
        .await
        .expect("the repo itself does not validate bodies");
    let tracks_before = boot
        .repo
        .tracks_by_area(&boot.area_id)
        .await
        .expect("list tracks")
        .len();

    let (status, error) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "from-late-header",
            json!({ "recipe_id": stored.id }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["code"], json!("bad_request"));
    assert!(
        error["error"]
            .as_str()
            .unwrap_or_default()
            .contains("first line"),
        "the header's own Misplaced message: {error}"
    );
    assert_eq!(
        boot.repo
            .tracks_by_area(&boot.area_id)
            .await
            .expect("list tracks")
            .len(),
        tracks_before,
        "no track was minted"
    );
}

/// KNOWN GAP: a non-canonical line 1 stored before normalization is a fail-closed 500, not a 400.
#[tokio::test]
async fn a_stored_recipe_with_a_non_canonical_header_is_a_fail_closed_500() {
    let boot = boot().await;
    let stored = boot
        .repo
        .track_recipe_create(calm_types::model::NewTrackRecipe {
            title: "pre-S2c header".into(),
            body: "<!-- neige:contract {\"sections\":[{\"omit_if_empty\":false,\"h1\":\"概要\"}],\"version\":1} -->\n\n# 概要\n".into(),
        })
        .await
        .expect("the repo itself does not validate bodies");
    let tracks_before = boot
        .repo
        .tracks_by_area(&boot.area_id)
        .await
        .expect("list tracks")
        .len();

    let (status, error) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "from-pre-s2c",
            json!({ "recipe_id": stored.id }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{error}");
    assert_eq!(error["code"], json!("internal"));
    assert!(
        error["error"]
            .as_str()
            .unwrap_or_default()
            .contains("non-canonical header reached the funnel"),
        "the funnel's own Internal message: {error}"
    );
    assert_eq!(
        boot.repo
            .tracks_by_area(&boot.area_id)
            .await
            .expect("list tracks")
            .len(),
        tracks_before,
        "fail-closed: no track was minted"
    );
}
