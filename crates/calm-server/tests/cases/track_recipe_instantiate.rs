//! #1292 S2 — creating a track from a user-defined recipe.
//!
//! S1 gave recipes storage and a write boundary. This is the other half: a
//! recipe becomes a track's initial report through the same seam a built-in
//! template uses (`prepare_initial_report_payload`), so the two differ only
//! in where the payload came from.
//!
//! What needs pinning here, and why:
//!
//!   * **The instantiated report equals the recipe.** Not "contains the
//!     title" — field by field on the task blocks, because a seam that
//!     dropped or rewrote a field would still produce a plausible report.
//!   * **Instantiation is a value copy.** Editing the recipe afterwards must
//!     not reach tracks already made from it, and editing such a track must
//!     not reach the recipe. Both directions, because either one leaking
//!     would make a recipe a live reference rather than a snapshot.
//!   * **`tracks.template_id` stays NULL.** A recipe id there would be
//!     resolved against plugin manifests on the track start path and log a
//!     failure for an entirely normal track.
//!   * **Two starting points is a 400**, not a silent winner.
//!
//! S3 adds provenance to the same file, since it is the same event being
//! observed from the other end — see the section header further down.

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
    /// The same repo, un-erased, so the provenance tests below can reach
    /// `pool()` and probe migration 0085's CHECK directly. Nothing else needs
    /// it: every other assertion here goes through a real read path.
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
///
/// `pub(crate)` since #1252 R1/F4: `track_report_fork` needs the *same* recipe
/// shape when it asserts that all three creation sources go through the
/// structural door with the same absences.
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

// ---------------------------------------------------------------------------

/// The instantiated report carries the recipe's tasks field for field.
///
/// Asserted per field rather than by comparing whole bodies: block ids and
/// revs are not a cross-implementation contract (#1300 §1.4 made that
/// explicit for the built-in path), but every *semantic* field is.
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

    let tasks = task_blocks(&payload);
    assert_eq!(tasks.len(), 2, "tasks={tasks:?}");
    assert_eq!(tasks[0]["key"], json!("setup"));
    assert_eq!(tasks[0]["goal"], json!("set the thing up"));
    assert_eq!(tasks[0]["acceptance"], json!("it is set up"));
    assert_eq!(tasks[1]["key"], json!("verify"));
    assert_eq!(tasks[1]["depends_on"], json!(["setup"]));

    // Normalized on the way *in* (S1), so it is already right here — the
    // instantiation seam re-normalizes nothing.
    for task in &tasks {
        assert_eq!(task["declared_by"], json!("spec"));
        assert_eq!(task["ready"], json!(false));
        assert!(task.get("released_by_user").is_none());
    }

    assert!(
        payload.body.contains("Set the thing up, then check it."),
        "prose survives: {}",
        payload.body
    );
}

/// A recipe whose *source* body carried `refs` instantiates to a task with
/// no reference, and therefore to no blocking diagnostic.
///
/// The write boundary already dropped the field (S1,
/// `normalize_recipe_body`), and `track_recipes.rs` asserts that on the
/// stored row. This asserts the same thing one layer down, where the cost
/// actually lands: `reference_missing` and `reference_cross_area` both carry
/// the `"refs"` diagnostic path, which is in
/// `TASK_BLOCKING_DIAGNOSTIC_PATHS`, so a surviving reference does not
/// merely look untidy in the report — it makes the instantiated task
/// unschedulable, in every track the recipe ever produces, until somebody
/// edits each one by hand.
///
/// The reference is deliberately dangling. A recipe can only ever ship a
/// block id it does not own — ids are minted per track at instantiation —
/// so "names a track that does not exist here" is the ordinary case, not a
/// contrived one.
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
            // So the only thing that can make this task unschedulable is the
            // reference — `schedulable` below is then a real assertion and
            // not one absorbed by an unrelated `gate_required`.
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
    let tasks = task_blocks(&payload);
    assert_eq!(tasks.len(), 1, "tasks={tasks:?}");
    assert!(
        tasks[0].get("refs").is_none(),
        "the instantiated task must carry no reference: {:?}",
        tasks[0]
    );
    // Not stripped, and the reason it is not: a path is a value its author
    // can mean, an id space that does not exist yet is not.
    assert_eq!(tasks[0]["cwd"], json!("/srv/repos/thing"));

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
    // Stronger, and reachable because the fixture gave the task a
    // `no_gate_reason`: the instantiated declaration raises *nothing*.
    //
    // Not `schedulable`, which is false here for a reason that has nothing
    // to do with references: S1 normalization sets `ready: false` on every
    // recipe task, so a recipe-created task is always waiting on its
    // planner. Asserting it would have been an assertion that can never go
    // green, and one that no mutation of this change could flip.
    assert!(
        verdicts
            .iter()
            .all(|verdict| verdict.diagnostics.is_empty()),
        "verdicts={verdicts:#?}"
    );
}

/// A recipe id must not land on `tracks.template_id`: the track start path
/// resolves that column against running plugins' manifests, and a recipe has
/// no manifest — every recipe-created track would log a resolution failure
/// for an entirely normal situation.
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

/// Instantiation is a value copy, asserted in **both** directions. Either
/// leak would make a recipe a live reference rather than a snapshot.
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

    // Edit the recipe out from under the track.
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
    let payload = report_payload(&track_detail(boot.app.clone(), &track_id).await);
    assert_eq!(payload.summary, "v1", "the track kept its snapshot");
    let keys: Vec<_> = task_blocks(&payload)
        .iter()
        .map(|task| task["key"].clone())
        .collect();
    assert_eq!(keys, vec![json!("setup"), json!("verify")]);

    // …and a *new* track picks up the edit, which is what makes the first
    // half meaningful: without this, "unchanged" could just mean the edit
    // never landed.
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
    let second_payload =
        report_payload(&track_detail(boot.app.clone(), second["id"].as_str().unwrap()).await);
    assert_eq!(second_payload.summary, "v2");
    assert_eq!(
        task_blocks(&second_payload)
            .iter()
            .map(|task| task["key"].clone())
            .collect::<Vec<_>>(),
        vec![json!("different")]
    );
}

/// Deleting a recipe leaves tracks made from it alone — they hold a copy,
/// not a reference.
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

    let payload = report_payload(&track_detail(boot.app.clone(), &track_id).await);
    assert_eq!(payload.summary, "doomed");
    assert_eq!(task_blocks(&payload).len(), 2);
}

/// A recipe deleted between the picker's read and the create is a 400 that
/// names the recipe, not a 500 and not a blank track.
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

/// Two starting points is not a preference to resolve.
///
/// The `code` assertion separates the refusal from a `500`/`internal` body — a
/// `panic!` on this path would produce exactly that, and asserting the status
/// alone would not tell the two apart.
///
/// 第一轮评审 MINOR-3 (#1321 S2) — this comment used to justify the `code`
/// assertion by a *second, local* refusal for the same combination inside the
/// `init` match in `create_track`. #1321 S2 deleted that match together with
/// its fallback arm (`NamedSource::from_request` is now the only refusal), so
/// the redundancy the sentence described no longer exists. The assertion is
/// still worth keeping for the reason above; it just is not guarding a second
/// guard any more.
///
/// 第一轮评审 MINOR-1 (#1321 S2) — the message assertion below is new. This is
/// the oldest of the four exclusivity cases (#1292) and it was the only one
/// asserting nothing about the body, which made it the one input on which the
/// design promise `NamedSource::from_request` writes down — *name the
/// conflicting fields, not "parameter conflict"* — had no pin. Two independent
/// review channels found it with the same construction: replace the computed
/// field list with a hard-coded `template_id`/`recipe_id`/`fork_report_from`
/// triple (or with one generic sentence) and every other exclusivity case goes
/// red while this one stays green. The negative half (`fork_report_from` must
/// **not** be named) is what makes it a pin on "the fields the caller actually
/// sent" rather than on "some fields".
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

/// All three named at once is the same 400.
///
/// Until #1321 S2 this case carried a narrower statement: `fork_report_from`
/// won over *one* named starting point but did not get to resolve a request
/// that named *two*, so ordering the fork arm first would have swallowed the
/// contradiction. With every pair refused there is no priority rule left to
/// outrank — what remains worth pinning is that the three-field request lands
/// on the *ambiguity* 400 and not on some later check, and that the message
/// still names the fields.
///
/// As above, the `code` assertion separates the refusal from a `500`/`internal`
/// panic body. 第一轮评审 MINOR-3 (#1321 S2) — it used to say "so this case
/// stays decisive **if the early guard is removed**", which named the same
/// deleted second refusal as the case above; there is no longer a second guard
/// for this one to survive the removal of. What the assertion still buys is
/// telling a refusal apart from a crash.
#[tokio::test]
async fn template_id_and_recipe_id_with_a_fork_source_are_still_a_400() {
    let boot = boot().await;
    let recipe = create_recipe(boot.app.clone(), "mine", &two_task_body()).await;

    // A real, forkable source track, so the request is rejected for its
    // ambiguity and not for a dangling fork source.
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

/// #1321 S2 — a `recipe_id` and a `fork_report_from` together are a 400.
///
/// ## Why this pair, when the issue only named the other one
///
/// This case replaces `an_explicit_fork_source_still_wins_over_a_recipe`, which
/// pinned the deleted behaviour: the fork used to win silently and the recipe
/// was dropped.
///
/// #1321 S2's acceptance names `template_id + fork_report_from`. The argument
/// #1292 wrote for `template_id + recipe_id` is what generalizes it: *a request
/// naming two starting points is ambiguous whether or not it also asks for a
/// fork, and ambiguity is not something a priority rule gets to resolve.* That
/// argument is about **naming two starting points**, not about which two — so
/// refusing only the pair the issue names would leave a hole of identical shape
/// one field over, in the field pair that this file, not the issue's, exercises.
///
/// The `summary` assertion is what makes this more than a status check: it
/// establishes the request was well-formed enough to have *succeeded* under the
/// old rule (a real recipe, a real forkable source), so the 400 is the
/// exclusivity and not a dangling id.
#[tokio::test]
async fn a_recipe_and_an_explicit_fork_source_are_a_400() {
    let boot = boot().await;
    let recipe = create_recipe(boot.app.clone(), "recipe-side", &two_task_body()).await;

    // A plain track whose report the old rule would have forked.
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

/// A zero-task recipe instantiates into a track with no tasks — legal all
/// the way down, and pinned so nobody adds a minimum-one-task rule at the
/// instantiation end either.
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

// ---------------------------------------------------------------------------
// #1292 S3 — provenance: which recipe, at which revision.
//
// S2 made instantiation a value copy. That is exactly what makes provenance a
// stored column rather than something derivable: once the recipe is edited or
// deleted there is nothing left to derive it from.
//
// The two column-list cases immediately below —
// `a_recipe_created_track_records_which_recipe_and_which_revision` and
// `the_track_detail_route_carries_the_provenance` — each run a real SELECT, one
// per constant. `TRACK_SELECT_COLUMNS` and `TRACK_SELECT_COLUMNS_W` are spliced
// into `query_as::<_, TrackRow>` SQL and bound by name at *runtime*, so a field
// added to `TrackRow` without the matching column in a list compiles clean and
// fails on the query. Comparing the two constants to each other cannot see that
// — they can be wrong together. Only executing the query can.
//
// The later cases in this section have other subjects, and one of them —
// `the_database_refuses_half_a_provenance` — runs no SELECT at all.
// ---------------------------------------------------------------------------

/// `repo.track_get` — the path built on `TRACK_SELECT_COLUMNS`.
///
/// # Why a constant-comparison test cannot replace this one
///
/// `calm_truth::db::rows::track_select_columns_lists_agree` compares
/// `TRACK_SELECT_COLUMNS` against `TRACK_SELECT_COLUMNS_W` and nothing else. It
/// defends the consistency of the two constants **with each other**, not their
/// consistency with `TrackRow` or with the `tracks` table — it never reads
/// either. Two lists that are wrong in the same way agree with each other
/// perfectly, so a field added to `TrackRow` and left out of both lists leaves
/// that test green and every `query_as::<_, TrackRow>` SELECT broken. The
/// binding happens by name at runtime, so the compiler is silent too.
///
/// Executing a SELECT is therefore the only thing in the repository that can
/// observe the failure, and this test is that execution for the unaliased
/// constant. Verified by mutation: deleting `recipe_id` from
/// `TRACK_SELECT_COLUMNS` turns this test red with `database error: no column
/// found for name: recipe_id`.
///
/// Reading the values back through a real query is also why this asserts on
/// `track_get` rather than on the `Track` the create call returned: that value
/// is built in memory by `track_create_tx` from what it just wrote, so it
/// would report the right answer with the column missing from every SELECT.
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

/// `GET /api/tracks/{id}` — the detail query, the one built on
/// `TRACK_SELECT_COLUMNS_W`.
///
/// A separate test from the one above on purpose: the aliased constant is a
/// second column list feeding a second SELECT, and a list that lost a column is
/// invisible until that particular query runs. Every case here that calls
/// `track_detail` executes the aliased constant; this is the case written for
/// it, and it reads the provenance columns back through it rather than the
/// report body. Same reasoning as the test above about why
/// comparing the two constants to each other cannot stand in for running the
/// query; verified by mutation, dropping `w.recipe_id` from
/// `TRACK_SELECT_COLUMNS_W` turns this red.
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

/// A track that came from anywhere else carries no origin at all.
///
/// Both columns, because "recorded for everything" and "recorded for recipes"
/// are different claims and only the second one is true.
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

/// Editing the recipe does not rewrite what an existing track records.
///
/// This is the whole reason the revision is stored rather than looked up: a
/// lookup would answer with today's revision, which is not the one the track
/// was built from. The second half — a track created *after* the edit records
/// the new revision — is what makes the first half mean "frozen" rather than
/// "always 1".
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

/// A deleted recipe leaves the id behind, and nothing about reading the track
/// breaks.
///
/// The id is deliberately not a foreign key and deliberately not cleared: "made
/// from a recipe that no longer exists" is the truthful answer, and blanking it
/// would replace a true statement with "made from nothing".
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

    // Both read paths, because a dangling id must break neither.
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

/// Migration 0085's cross-column CHECK, exercised in both directions.
///
/// The `track_create_tx` parameter is a single `Option<TrackRecipeOrigin>`, so
/// no caller of *that* writer can produce half a provenance — which is exactly
/// why this test writes straight at the database instead. The CHECK is the
/// fence for every other writer there will ever be: a later PATCH branch, a
/// backfill migration, a hand-run UPDATE. It only earns its keep if the
/// database is the thing refusing, and the only way to see that is to ask the
/// database.
///
/// The two UPDATEs run against a row the real create path produced, so both
/// column names and the constraint under test are the ones production uses,
/// not a re-spelling of the create. The INSERT below them adds no
/// mutation-catching power — SQLite evaluates the same CHECK expression on both
/// paths, so nothing can break the INSERT direction alone — and is here only so
/// that "does the fence stand in front of new rows too" is answered where it is
/// asked.
///
/// Each assertion names the constraint rather than matching bare
/// `"CHECK constraint failed"`: `tracks` also carries the CHECK 0071 added,
/// today reading `parent_track_id IS NULL OR parent_track_id <> id`, so the
/// bare substring is satisfiable by a constraint that is not the one under
/// test. SQLite reports the declared name in the error text, which is why 0085
/// declares one.
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

    // An id with no revision.
    let error = sqlx::query("UPDATE tracks SET recipe_revision = NULL WHERE id = ?1")
        .bind(&track_id)
        .execute(pool)
        .await
        .expect_err("a recipe id with no revision must be refused");
    assert!(
        error.to_string().contains(REFUSED_BY),
        "expected {REFUSED_BY} to be what refused it, got: {error}"
    );

    // A revision naming no recipe.
    let error = sqlx::query("UPDATE tracks SET recipe_id = NULL WHERE id = ?1")
        .bind(&track_id)
        .execute(pool)
        .await
        .expect_err("a revision with no recipe id must be refused");
    assert!(
        error.to_string().contains(REFUSED_BY),
        "expected {REFUSED_BY} to be what refused it, got: {error}"
    );

    // And on the INSERT direction. SQLite evaluates the same expression on both
    // paths, so this catches no mutation the UPDATEs above miss; it is here
    // because "the fence also stands in front of new rows" is the thing a
    // reader wants answered, and answering it costs three lines.
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

    // Clearing both together is the one legal way out, and it stays legal.
    sqlx::query("UPDATE tracks SET recipe_id = NULL, recipe_revision = NULL WHERE id = ?1")
        .bind(&track_id)
        .execute(pool)
        .await
        .expect("clearing both at once is a state the system has a reading for");
}

/// A fork of a recipe-born track records no provenance of its own.
///
/// That is a decision made by omission — the fork arm of
/// `create_track_with_planner_harness` never produces a `TrackRecipeOrigin` —
/// which is why it is pinned rather than left to the columns' defaults.
///
/// #1321 S2 — this case used to pin a second half in the same body: that a
/// request naming both a `recipe_id` and a `fork_report_from` resolved to the
/// fork rather than to a 400. That half is deleted, not rewritten: the
/// combination is now the 400 that
/// `a_recipe_and_an_explicit_fork_source_are_a_400` above pins. Note what the
/// deleted half is *not* — it was never needed to reach this case's own
/// property. The recipe provenance being tested belongs to the **source**
/// track, and this fork now names only `fork_report_from`, which is the shape
/// every production fork has always had (neither frontend has ever sent
/// `fork_report_from` at all, let alone with a second source alongside it).
///
/// The forked report is asserted first so "no provenance" is not read as "the
/// content did not arrive either": it did arrive, one hop removed, and the
/// columns still stay NULL, because they name the recipe a track was
/// *instantiated* from and this track was instantiated from a track.
///
/// Mutations this catches. Bound but not separately run: a fork arm that
/// stamped provenance would break the two NULL assertions below, and no other
/// case in this file asserts on a fork's provenance columns. (The mutation the
/// previous version named — adding a `(_, Some(_), Some(_))` 400 arm ahead of
/// the fork arm — is no longer a mutation; it is the behaviour.)
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

    // The source does carry provenance — otherwise the fork having none below
    // would prove nothing about the fork.
    let source_track = boot
        .repo
        .track_get(&source_id)
        .await
        .expect("track_get")
        .expect("source exists");
    assert_eq!(source_track.recipe_id.as_deref(), Some(recipe_id.as_str()));

    // Fork it — naming only the fork source, the one shape a create may name.
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
    let payload = report_payload(&track_detail(boot.app.clone(), &fork_id).await);
    assert_eq!(payload.summary, "forkable");
    let keys: Vec<_> = task_blocks(&payload)
        .iter()
        .map(|task| task["key"].clone())
        .collect();
    assert_eq!(keys, vec![json!("setup"), json!("verify")]);

    // …and records no origin anyway.
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
