//! What `template_id` does on `POST /api/tracks`.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{EditAuthor, EventBus};
use calm_server::ids::ActorId;
use calm_server::model::{NewArea, NewOverlay, NewPlugin};
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use calm_server::track_report::{TrackReportPayload, persist_report, resolve_report_for_track};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::common;
use crate::support::git_helpers::attached_repo_fixture;

const ISSUE_DEVELOPMENT: &str = "issue-development";
const SMALL_CHANGE: &str = "small-change";
const INVESTIGATION: &str = "investigation";
const INVESTMENT_RESEARCH: &str = "investment-research";

struct Boot {
    app: axum::Router,
    state: AppState,
    area_id: String,
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
    let area = repo
        .area_create(NewArea {
            name: "template-template-test".into(),
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
            std::env::temp_dir().join("calm-plugins-data-1110-s6"),
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
        .with_state(state.clone());
    Boot {
        app,
        state,
        area_id: area.id.to_string(),
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

/// Like [`post`], but returns the body as raw text: extractor-level rejections are `text/plain`, not the JSON envelope.
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

async fn planner_harness_ops_for_track(repo: &Arc<dyn Repo>, track_id: &str) -> i64 {
    let pool = repo.sqlite_pool().expect("sqlite pool");
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM operations \
         WHERE kind = 'planner-harness-start' \
           AND json_extract(payload_json, '$.wave_id') = ?1",
    )
    .bind(track_id)
    .fetch_one(&pool)
    .await
    .expect("track planner-harness-start count")
}

fn create_body(area_id: &str, title: &str, extra: Value) -> Value {
    let mut body = json!({
        "area_id": area_id,
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

/// A whole-database snapshot: every user table, rendered through SQLite's `quote()`, ordered so the digest is stable.
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

/// Every `kernel` / `view` / `template` overlay, whatever its payload shape, as `(entity_id, payload)`.
async fn kernel_template_overlays(repo: &Arc<dyn Repo>) -> Vec<(String, Value)> {
    let overlays = repo
        .overlays_by_kind("view")
        .await
        .expect("template overlays");
    let mut found: Vec<(String, Value)> = overlays
        .into_iter()
        .filter(|overlay| overlay.plugin_id == "kernel" && overlay.kind == "template")
        .map(|overlay| (overlay.entity_id, overlay.payload))
        .collect();
    found.sort_by(|left, right| left.0.cmp(&right.0));
    found
}

fn report_card_payload(detail: &Value) -> TrackReportPayload {
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

#[tokio::test]
async fn creating_from_a_template_mints_no_hidden_track() {
    let boot = boot().await;

    let tracks_before = boot
        .repo
        .tracks_window(None, None, None)
        .await
        .unwrap()
        .len();

    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        create_body(
            &boot.area_id,
            "template only",
            json!({ "template_id": ISSUE_DEVELOPMENT }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let created = body["id"].as_str().expect("track id").to_string();

    let after = boot.repo.tracks_window(None, None, None).await.unwrap();
    assert_eq!(
        after.len(),
        tracks_before + 1,
        "created {} tracks, not 1",
        after.len() - tracks_before
    );
    assert!(
        after.iter().any(|track| track.id.as_str() == created),
        "the requested track is not among them"
    );

    assert!(
        kernel_template_overlays(&boot.repo).await.is_empty(),
        "a kernel/view/template overlay was minted; the kernel has \
         no writer for one: {:?}",
        kernel_template_overlays(&boot.repo).await
    );
    assert!(
        boot.repo.area_get_system().await.unwrap().is_none(),
        "creating from a template must not mint the system area either"
    );
}

/// Edits one track and re-reads the other; each leg targets a different fan-out guard shape.
#[tokio::test]
async fn two_tracks_from_one_template_are_independent_and_identical() {
    let boot = boot().await;
    let mut reports = Vec::new();
    for leg in ["first", "second"] {
        let (status, body) = post(
            boot.app.clone(),
            "/api/tracks",
            create_body(&boot.area_id, leg, json!({ "template_id": SMALL_CHANGE })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{leg}: body={body}");
        let track_id = body["id"].as_str().expect("track id").to_string();
        let (_, detail) = get(boot.app.clone(), &format!("/api/tracks/{track_id}")).await;
        reports.push((track_id, report_card_payload(&detail)));
    }
    let [(first_id, first), (second_id, second)] = <[_; 2]>::try_from(reports).ok().unwrap();

    assert_ne!(first_id, second_id, "two creates must be two tracks");
    assert_eq!(first.summary, second.summary);
    assert_eq!(first.body, second.body);

    // Independence: edit one, re-read the other.
    const EDITED: &str = "first track's own summary";
    const APPENDED: &str = "A paragraph only the first track's author wrote.";
    assert_ne!(
        first.summary, EDITED,
        "the edit must change something, or the re-read below asserts nothing"
    );
    assert!(
        !first.body.contains(APPENDED),
        "the body edit must change something, or the re-read below asserts nothing"
    );
    // Appended after the last task fence: every non-prose block travels
    // through byte-identical, which is all `guard_non_prose_stomp` asks.
    let edited_body = format!("{}\n\n{APPENDED}\n", first.body.trim_end());
    let (source_track, report_card, current) =
        resolve_report_for_track(boot.repo.as_ref(), &first_id)
            .await
            .expect("first track's report");
    let if_doc_rev = current.doc_rev;
    persist_report(
        boot.repo.as_ref(),
        &boot.state.events,
        boot.state.write(),
        ActorId::User,
        EditAuthor::User,
        source_track,
        report_card,
        current,
        TrackReportPayload::new(EDITED, edited_body.clone()),
        if_doc_rev,
        None,
        None,
        false,
    )
    .await
    .expect("edit the first track's report");

    let (status, first_detail) = get(boot.app.clone(), &format!("/api/tracks/{first_id}")).await;
    assert_eq!(status, StatusCode::OK, "detail={first_detail}");
    let first_after = report_card_payload(&first_detail);
    assert_eq!(
        first_after.summary, EDITED,
        "the summary edit did not land, so the re-read below proves nothing"
    );
    assert!(
        first_after.body.contains(APPENDED),
        "the body edit did not land, so the re-read below proves nothing; \
         body={}",
        first_after.body
    );

    let (status, second_detail) = get(boot.app.clone(), &format!("/api/tracks/{second_id}")).await;
    assert_eq!(status, StatusCode::OK, "detail={second_detail}");
    let second_after = report_card_payload(&second_detail);
    assert_eq!(
        second_after.summary, second.summary,
        "editing one template-created track changed the other's summary: the two \
         tracks share a document"
    );
    assert_eq!(
        second_after.body, second.body,
        "editing one template-created track changed the other's body: the two \
         tracks share a document"
    );

    // Second edit: same length, no new text.
    const REPLACED: &str = "A paragraph only the first track's author typed.";
    assert_eq!(
        APPENDED.len(),
        REPLACED.len(),
        "this leg is the equal-length one; if these two ever differ it silently \
         degrades into a second append"
    );
    let same_len_body = first_after.body.replace(APPENDED, REPLACED);
    assert_eq!(
        same_len_body.len(),
        first_after.body.len(),
        "the replacement changed the document length, so this leg no longer \
         exercises the equal-length branch"
    );
    assert_ne!(
        same_len_body, first_after.body,
        "the replacement must change something, or the re-read below asserts nothing"
    );
    let (source_track, report_card, current) =
        resolve_report_for_track(boot.repo.as_ref(), &first_id)
            .await
            .expect("first track's report, again");
    let if_doc_rev = current.doc_rev;
    persist_report(
        boot.repo.as_ref(),
        &boot.state.events,
        boot.state.write(),
        ActorId::User,
        EditAuthor::User,
        source_track,
        report_card,
        current,
        TrackReportPayload::new(EDITED, same_len_body.clone()),
        if_doc_rev,
        None,
        None,
        false,
    )
    .await
    .expect("same-length edit to the first track's report");

    let (status, first_detail) = get(boot.app.clone(), &format!("/api/tracks/{first_id}")).await;
    assert_eq!(status, StatusCode::OK, "detail={first_detail}");
    let first_replaced = report_card_payload(&first_detail);
    assert!(
        first_replaced.body.contains(REPLACED),
        "the same-length edit did not land, so the re-read below proves nothing; \
         body={}",
        first_replaced.body
    );

    let (status, second_detail) = get(boot.app.clone(), &format!("/api/tracks/{second_id}")).await;
    assert_eq!(status, StatusCode::OK, "detail={second_detail}");
    let second_replaced = report_card_payload(&second_detail);
    assert!(
        !second_replaced.body.contains(REPLACED),
        "a same-length edit to one template-created track reached the other's \
         body: the two tracks share a document; body={}",
        second_replaced.body
    );
    assert_eq!(
        second_replaced.body, second.body,
        "a same-length edit to one template-created track changed the other's \
         body: the two tracks share a document"
    );
    assert_eq!(
        second_replaced.summary, second.summary,
        "a same-length edit to one template-created track changed the other's \
         summary: the two tracks share a document"
    );

    // Third edit: a block the template itself minted.
    const RECIPE_PROSE: &str = "Short inspect";
    const RECIPE_PROSE_EDITED: &str = "Quick inspect";
    assert!(
        first_replaced.body.contains(RECIPE_PROSE),
        "the recipe's intro prose is not in the body, so this leg edits nothing; \
         body={}",
        first_replaced.body
    );
    let recipe_edited_body = first_replaced
        .body
        .replace(RECIPE_PROSE, RECIPE_PROSE_EDITED);
    let (source_track, report_card, current) =
        resolve_report_for_track(boot.repo.as_ref(), &first_id)
            .await
            .expect("first track's report, third time");
    let if_doc_rev = current.doc_rev;
    persist_report(
        boot.repo.as_ref(),
        &boot.state.events,
        boot.state.write(),
        ActorId::User,
        EditAuthor::User,
        source_track,
        report_card,
        current,
        TrackReportPayload::new(EDITED, recipe_edited_body.clone()),
        if_doc_rev,
        None,
        None,
        false,
    )
    .await
    .expect("edit a template-minted prose block of the first track's report");

    let (status, first_detail) = get(boot.app.clone(), &format!("/api/tracks/{first_id}")).await;
    assert_eq!(status, StatusCode::OK, "detail={first_detail}");
    let first_recipe_edited = report_card_payload(&first_detail);
    assert!(
        first_recipe_edited.body.contains(RECIPE_PROSE_EDITED),
        "the recipe-prose edit did not land, so the re-read below proves nothing; \
         body={}",
        first_recipe_edited.body
    );

    let (status, second_detail) = get(boot.app.clone(), &format!("/api/tracks/{second_id}")).await;
    assert_eq!(status, StatusCode::OK, "detail={second_detail}");
    let second_recipe_edited = report_card_payload(&second_detail);
    assert_eq!(
        second_recipe_edited.body, second.body,
        "editing a template-minted block of one track changed the other's body: \
         the two tracks share a document"
    );

    // Fourth edit: the body gets shorter.
    let appended_suffix = format!("\n\n{REPLACED}\n");
    let shortened_body = first_recipe_edited
        .body
        .strip_suffix(&appended_suffix)
        .unwrap_or_else(|| {
            panic!(
                "the appended paragraph is not the body's suffix, so this leg \
                 would not be the shrinking one; body={}",
                first_recipe_edited.body
            )
        })
        .to_string();
    assert!(
        shortened_body.len() < first_recipe_edited.body.len(),
        "this leg is the shrinking one; if it stops shrinking it silently \
         degrades into a repeat of leg three"
    );
    let (source_track, report_card, current) =
        resolve_report_for_track(boot.repo.as_ref(), &first_id)
            .await
            .expect("first track's report, fourth time");
    let if_doc_rev = current.doc_rev;
    persist_report(
        boot.repo.as_ref(),
        &boot.state.events,
        boot.state.write(),
        ActorId::User,
        EditAuthor::User,
        source_track,
        report_card,
        current,
        TrackReportPayload::new(EDITED, shortened_body.clone()),
        if_doc_rev,
        None,
        None,
        false,
    )
    .await
    .expect("delete a prose paragraph from the first track's report");

    let (status, first_detail) = get(boot.app.clone(), &format!("/api/tracks/{first_id}")).await;
    assert_eq!(status, StatusCode::OK, "detail={first_detail}");
    let first_shortened = report_card_payload(&first_detail);
    assert!(
        !first_shortened.body.contains(REPLACED),
        "the deleting edit did not land, so the re-read below proves nothing; \
         body={}",
        first_shortened.body
    );
    assert!(
        first_shortened.body.len() < first_recipe_edited.body.len(),
        "the deleting edit did not shorten the document, so this leg no longer \
         exercises the shrink branch; body={}",
        first_shortened.body
    );

    let (status, second_detail) = get(boot.app.clone(), &format!("/api/tracks/{second_id}")).await;
    assert_eq!(status, StatusCode::OK, "detail={second_detail}");
    let second_shortened = report_card_payload(&second_detail);
    assert_eq!(
        second_shortened.body, second.body,
        "a deleting edit to one template-created track changed the other's body: \
         the two tracks share a document"
    );
    assert_eq!(
        second_shortened.summary, second.summary,
        "a deleting edit to one template-created track changed the other's \
         summary: the two tracks share a document"
    );
}

#[tokio::test]
async fn issue_development_create_forks_inspect_issue_not_ready() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        create_body(
            &boot.area_id,
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
    let track_id = body["id"].as_str().expect("track id");
    assert!(
        planner_harness_ops_for_track(&boot.repo, track_id).await >= 1,
        "forked user track still starts planner harness"
    );

    let (status, detail) = get(boot.app.clone(), &format!("/api/tracks/{track_id}")).await;
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
        "implement-change must tell planner to author a real gate; payload={implement}"
    );
}

#[tokio::test]
async fn a_template_and_an_explicit_fork_source_are_a_400() {
    let boot = boot().await;
    let (status, source) = post(
        boot.app.clone(),
        "/api/tracks",
        create_body(&boot.area_id, "custom-source", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={source}");
    let source_id = source["id"].as_str().unwrap().to_string();
    // The source track's planner is stopped so its observation-queue persistence cannot race the snapshots.
    let source_worker_session_id: String = sqlx::query_scalar(
        "SELECT id FROM worker_sessions WHERE track_id=?1 ORDER BY created_at_ms DESC LIMIT 1",
    )
    .bind(&source_id)
    .fetch_one(&boot.repo.sqlite_pool().expect("sqlite pool"))
    .await
    .expect("source planner runtime");
    let source_harness = boot
        .state
        .harness
        .remove(&source_worker_session_id)
        .expect("source planner harness");
    source_harness
        .shutdown()
        .await
        .expect("shutdown source planner harness");
    let (source_track, report_card, current) =
        resolve_report_for_track(boot.repo.as_ref(), &source_id)
            .await
            .expect("source report");
    let if_doc_rev = current.doc_rev;
    persist_report(
        boot.repo.as_ref(),
        &boot.state.events,
        boot.state.write(),
        ActorId::User,
        EditAuthor::User,
        source_track,
        report_card,
        current,
        TrackReportPayload::new(
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

    // Leg 1 — naming both is refused, and nothing is written deciding that.
    let before = db_snapshot(&boot.repo).await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        create_body(
            &boot.area_id,
            "template-plus-explicit-fork",
            json!({
                "template_id": ISSUE_DEVELOPMENT,
                "fork_report_from": source_id,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert_eq!(body["code"], json!("bad_request"), "body={body}");
    let error = body["error"].as_str().unwrap_or("");
    assert!(
        error.contains("`template_id`") && error.contains("`fork_report_from`"),
        "the 400 must name both offending fields; body={body}"
    );
    assert!(
        !error.contains("`recipe_id`"),
        "it must name the fields that were actually sent; body={body}"
    );
    assert_eq!(
        db_snapshot(&boot.repo).await,
        before,
        "an ambiguous-source 400 must not write anything"
    );

    // Leg 2 — a fork-only create still copies the source's edited report.
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        create_body(
            &boot.area_id,
            "explicit-fork",
            json!({ "fork_report_from": source_id }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    assert!(
        body["template_id"].is_null(),
        "a fork create records no template provenance; body={body}"
    );
    let track_id = body["id"].as_str().expect("track id");
    let (status, detail) = get(boot.app.clone(), &format!("/api/tracks/{track_id}")).await;
    assert_eq!(status, StatusCode::OK, "detail={detail}");
    let payload = report_card_payload(&detail);
    assert!(
        payload.body.contains("not-the-issue-development-plan"),
        "the fork must carry the source's edited report; body={}",
        payload.body
    );
    assert!(
        !payload.body.contains("inspect-issue"),
        "no template plan may be grafted onto a fork; body={}",
        payload.body
    );
}

#[tokio::test]
async fn investigation_and_small_change_auto_fork_without_plugin() {
    let boot = boot().await;
    for (key, task_key) in [(SMALL_CHANGE, "inspect"), (INVESTIGATION, "gather-facts")] {
        let (status, body) = post(
            boot.app.clone(),
            "/api/tracks",
            create_body(
                &boot.area_id,
                &format!("forked-{key}"),
                json!({ "template_id": key }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "key={key} body={body}");
        assert_eq!(body["template_id"], key);
        assert!(body["plugin_scope"].is_null());
        let track_id = body["id"].as_str().unwrap();
        let (status, detail) = get(boot.app.clone(), &format!("/api/tracks/{track_id}")).await;
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
async fn a_forged_template_key_cannot_influence_what_a_template_creates() {
    let boot = boot().await;

    // The forgery: a track in the user's area, wearing the `issue-development` key.
    let (status, stolen) = post(
        boot.app.clone(),
        "/api/tracks",
        create_body(&boot.area_id, "forged-template", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={stolen}");
    let stolen_id = stolen["id"].as_str().unwrap().to_string();
    // `POST /api/overlays` refuses the reserved `kernel` / `view` namespaces; assert the cheap layer first.
    let (status, refused) = post(
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
    assert_eq!(status, StatusCode::FORBIDDEN, "body={refused}");

    // Then plant the stolen key anyway, bypassing the route, so the deeper invariant is still exercised.
    boot.repo
        .overlay_upsert(NewOverlay {
            plugin_id: "kernel".into(),
            entity_kind: "view".into(),
            entity_id: stolen_id.clone(),
            kind: "template".into(),
            payload: json!({
                "schemaVersion": 1,
                "template_key": ISSUE_DEVELOPMENT,
            }),
        })
        .await
        .expect("plant stolen template_key");
    let (stolen_track, report_card, current) =
        resolve_report_for_track(boot.repo.as_ref(), &stolen_id)
            .await
            .expect("forged report");
    let if_doc_rev = current.doc_rev;
    persist_report(
        boot.repo.as_ref(),
        &boot.state.events,
        boot.state.write(),
        ActorId::User,
        EditAuthor::User,
        stolen_track,
        report_card,
        current,
        TrackReportPayload::new("forged template", "# Forged\n\nforged-user-area-plan\n"),
        if_doc_rev,
        None,
        None,
        false,
    )
    .await
    .expect("stamp forged report");

    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        create_body(
            &boot.area_id,
            "after-forged-key",
            json!({ "template_id": ISSUE_DEVELOPMENT }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track_id = body["id"].as_str().expect("track id");
    let (status, detail) = get(boot.app.clone(), &format!("/api/tracks/{track_id}")).await;
    assert_eq!(status, StatusCode::OK, "detail={detail}");
    let payload = report_card_payload(&detail);

    let (summary, expected_body, _) = instantiated_recipe(ISSUE_DEVELOPMENT);
    assert_eq!(payload.summary, summary);
    assert_eq!(
        payload.body, expected_body,
        "a forged template_key must not reach the created report"
    );
}

#[tokio::test]
async fn create_stores_the_roster_key_as_template_id() {
    let boot = boot().await;
    for key in calm_server::templates::TemplateRoster::builtin()
        .entries()
        .iter()
        .map(|t| t.key())
    {
        let (status, body) = post(
            boot.app.clone(),
            "/api/tracks",
            create_body(&boot.area_id, key, json!({ "template_id": key })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{key}: body={body}");
        let track_id = body["id"].as_str().expect("track id").to_string();
        let track = boot
            .repo
            .track_get(&track_id)
            .await
            .expect("track_get")
            .expect("created track row");
        assert_eq!(
            track.template_id.as_deref(),
            Some(key),
            "{key}: tracks.template_id must carry the roster key"
        );
    }

    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        create_body(&boot.area_id, "no template", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track_id = body["id"].as_str().expect("track id").to_string();
    let track = boot
        .repo
        .track_get(&track_id)
        .await
        .expect("track_get")
        .expect("created track row");
    assert_eq!(track.template_id, None, "unbound create must store NULL");
}

#[tokio::test]
async fn unknown_template_id_still_400s() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app,
        "/api/tracks",
        create_body(
            &boot.area_id,
            "unknown-template",
            json!({ "template_id": "missing-template" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    let error = body["error"].as_str().unwrap_or("");
    assert!(error.contains("known track template"), "body={body}");
    assert!(
        !error.contains("registered trusted template"),
        "body={body}"
    );
    assert!(error.contains("missing-template"), "body={body}");
}

/// A running, trusted plugin declaring the given ids; no `input_schema` (its `required` list would 400 every
/// create) and a separate boot from `boot()`, which starts no plugins.
async fn boot_with_trusted_plugin(declared_template_ids: &[&str]) -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let area = repo
        .area_create(NewArea {
            name: "template-template-plugin-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();

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
            // No `input_schema`: its `required` list would 400 every create without `template_input`.
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
        calm_server::state::WriteContext::new(card_role_cache.clone(), track_area_cache.clone()),
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
        Some(track_area_cache),
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
        area_id: area.id.to_string(),
        repo,
        _tmp: tmp,
    }
}

#[tokio::test]
async fn plugin_declared_non_template_id_is_rejected() {
    const NOT_A_TEMPLATE: &str = "not-a-template";
    let boot = boot_with_trusted_plugin(&[NOT_A_TEMPLATE, ISSUE_DEVELOPMENT]).await;

    // Liveness control: proves the plugin really binds on this app, so the rejection below is not for an unbound id.
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        create_body(
            &boot.area_id,
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
        "/api/tracks",
        create_body(
            &boot.area_id,
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
    assert!(error.contains("known track template"), "body={body}");
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
    assert_eq!(
        db_snapshot(&boot.repo).await,
        before,
        "an admission 400 must not write anything"
    );
}

/// Carries a valid `area_id`, no `cwd` and no `template_input`, so no other validation can supply the 400.
#[tokio::test]
async fn blank_template_id_is_rejected() {
    let boot = boot().await;
    let before = db_snapshot(&boot.repo).await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.area_id,
            "title": "blank template id",
            "theme": theme(),
            "template_id": "   ",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    let error = body["error"].as_str().unwrap_or("");
    assert!(error.contains("known track template"), "body={body}");
    assert!(error.contains("got `   `"), "body={body}");
    assert_eq!(
        db_snapshot(&boot.repo).await,
        before,
        "a rejected create must not seed"
    );
}

/// Split into one test per leg: a single loop short-circuits on the first failing leg.
async fn assert_pre_transaction_4xx_does_not_seed(
    name: &str,
    body_json: Value,
    expected: StatusCode,
) {
    let boot = boot().await;
    let mut body_json = body_json;
    if body_json["area_id"] == json!("") {
        body_json["area_id"] = json!(boot.area_id);
    }
    let before = db_snapshot(&boot.repo).await;
    let (status, body) = post(boot.app.clone(), "/api/tracks", body_json).await;
    assert_eq!(status, expected, "{name}: body={body}");
    assert!(
        kernel_template_overlays(&boot.repo).await.is_empty(),
        "{name}: a pre-transaction 4xx seeded template tracks"
    );
    assert_eq!(
        db_snapshot(&boot.repo).await,
        before,
        "{name}: a pre-transaction 4xx wrote to the database"
    );
}

#[tokio::test]
async fn pre_transaction_404_unknown_area_with_template_does_not_seed() {
    assert_pre_transaction_4xx_does_not_seed(
        "area 404",
        json!({
            "area_id": "area_does_not_exist",
            "title": "unknown area",
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
            "area_id": "",
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
    // An absolute, existing directory that is not a git repository; `attach_folder` stays false because the guard keys off `cwd` alone.
    let non_repo = TempDir::new().expect("non-repo tempdir");
    assert_pre_transaction_4xx_does_not_seed(
        "cwd is not a git repository",
        json!({
            "area_id": "",
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

#[tokio::test]
async fn listing_templates_returns_constants_and_writes_nothing() {
    let boot = boot().await;

    // Two states: the second leg runs the read against a populated database.
    for leg in ["empty database", "after a create"] {
        let before = db_snapshot(&boot.repo).await;
        let (status, body) = get(boot.app.clone(), "/api/track-templates").await;
        assert_eq!(status, StatusCode::OK, "{leg}: body={body}");
        let listed_ids: Vec<&str> = body
            .as_array()
            .expect("array body")
            .iter()
            .map(|entry| entry["id"].as_str().expect("template id"))
            .collect();
        let roster_ids: Vec<&str> = calm_server::templates::TemplateRoster::builtin()
            .entries()
            .iter()
            .map(|template| template.key())
            .collect();
        assert_eq!(
            listed_ids, roster_ids,
            "{leg}: the listing is the roster, in the roster's order"
        );
        // `investment-research` is the one report-only template: its tasks array is present and empty.
        for entry in body.as_array().expect("array body") {
            assert!(
                entry["title"].as_str().is_some_and(|t| !t.is_empty()),
                "{leg}: {entry} has no title"
            );
            assert!(
                entry["tasks"].is_array(),
                "{leg}: {entry} carries no tasks array"
            );
            assert_eq!(
                task_keys(entry).is_empty(),
                entry["id"] == INVESTMENT_RESEARCH,
                "{leg}: {entry} advertises tasks iff it is a plan template"
            );
        }
        assert_eq!(
            db_snapshot(&boot.repo).await,
            before,
            "{leg}: listing templates wrote to the database"
        );

        if leg == "empty database" {
            let (status, created) = post(
                boot.app.clone(),
                "/api/tracks",
                create_body(
                    &boot.area_id,
                    "populate",
                    json!({ "template_id": SMALL_CHANGE }),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "body={created}");
        }
    }
}

/// `CreateTrackRequest` carries `#[serde(deny_unknown_fields)]`, so serde's own wording is expected and the admission wording must be absent.
async fn assert_old_spelling_is_an_unknown_field(leg: &str, body_json: Value, unknown_key: &str) {
    let boot = boot().await;
    let before = db_snapshot(&boot.repo).await;
    let (status, text) = post_text(boot.app.clone(), "/api/tracks", body_json).await;
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
        !text.contains("known track template"),
        "{leg}: an admission-flavoured rejection would mean \
         `CreateTrackRequest` declares the old key again, body={text}"
    );
    assert_eq!(
        db_snapshot(&boot.repo).await,
        before,
        "{leg}: a rejected create must not leave a persisted row behind"
    );
}

#[tokio::test]
async fn old_template_id_spelling_is_an_unknown_field() {
    assert_old_spelling_is_an_unknown_field(
        "row 18: workflow_id alone",
        json!({
            "area_id": "",
            "title": "old id spelling",
            "attach_folder": false,
            "theme": theme(),
            "workflow_id": SMALL_CHANGE,
        }),
        "workflow_id",
    )
    .await;
}

#[tokio::test]
async fn old_template_input_spelling_is_an_unknown_field() {
    assert_old_spelling_is_an_unknown_field(
        "row 19: new template_id + old workflow_input",
        json!({
            "area_id": "",
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

/// The retired `as_template` field; the observed status is `422` from axum's `Json` extractor, not `400`.
#[tokio::test]
async fn retired_as_template_is_an_unknown_field() {
    assert_old_spelling_is_an_unknown_field(
        "#1318 S2: as_template retired",
        json!({
            "area_id": "",
            "title": "retired as_template",
            "attach_folder": false,
            "theme": theme(),
            "as_template": true,
        }),
        "as_template",
    )
    .await;
}

#[tokio::test]
async fn both_spellings_together_are_an_unknown_field() {
    assert_old_spelling_is_an_unknown_field(
        "row 20: template_id and workflow_id together",
        json!({
            "area_id": "",
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

/// The report a template must instantiate to, derived from the recipe; non-task slices are carried through byte for byte.
fn instantiated_recipe(key: &str) -> (String, String, Vec<Value>) {
    use calm_types::report_blocks::{KIND_TASK, parse_fence, render_fence, split_body};

    let recipe = calm_server::templates::TemplateRoster::builtin()
        .get(key)
        .unwrap_or_else(|| panic!("`{key}` is not a known template"))
        .recipe();
    let mut body = String::new();
    let mut tasks = Vec::new();
    for slice in split_body(&recipe.body) {
        match parse_fence(&slice.raw) {
            Some(fence) if fence.kind == KIND_TASK => {
                let mut payload = fence.payload;
                payload["declared_by"] = json!("spec");
                payload["ready"] = json!(false);
                body.push_str(&render_fence(KIND_TASK, &payload));
                tasks.push(payload);
            }
            _ => body.push_str(&slice.raw),
        }
    }
    (recipe.summary, body, tasks)
}

/// `boot()` deliberately starts no plugins, so plugin input validation cannot turn a listed template into an unrelated 400.
#[tokio::test]
async fn listed_template_keys_create_their_exact_recipes() {
    // key, roster title, ordered task keys. Hand-written on purpose — this is
    // the one table in this file that must NOT be derived from production.
    let anchors: [(&str, &str, &[&str]); 4] = [
        (
            ISSUE_DEVELOPMENT,
            "Issue development",
            &[
                "inspect-issue",
                "review-design-a",
                "review-design-b",
                "implement-change",
                "open-pr",
                "review-pr-a",
                "review-pr-b",
                "merge",
            ],
        ),
        (
            SMALL_CHANGE,
            "Small change",
            &["inspect", "implement", "verify"],
        ),
        (
            INVESTIGATION,
            "Investigation",
            &["gather-facts", "write-findings"],
        ),
        // A report-only template: no pre-set tasks. The empty key list is the anchor.
        (INVESTMENT_RESEARCH, "Investment research", &[]),
    ];
    assert_eq!(
        anchors.len(),
        calm_server::templates::TemplateRoster::builtin()
            .entries()
            .len(),
        "the roster grew or shrank; this table is the one place that must be \
         edited by hand when it does"
    );

    let boot = boot().await;
    let (status, listing) = get(boot.app.clone(), "/api/track-templates").await;
    assert_eq!(status, StatusCode::OK, "listing={listing}");
    assert_eq!(
        listing.as_array().expect("template listing array").len(),
        anchors.len(),
        "the picker and the hand-written roster must describe the same set"
    );

    for (key, title, expected_task_keys) in anchors {
        // Road 1 — the picker read.
        let listed = listed_template(&listing, key);
        assert_eq!(listed["title"], title, "{key}: picker title");
        assert_eq!(
            task_keys(listed),
            expected_task_keys.to_vec(),
            "{key}: picker tasks"
        );

        // Road 2 — the create write.
        let (status, body) = post(
            boot.app.clone(),
            "/api/tracks",
            create_body(&boot.area_id, key, json!({ "template_id": key })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{key}: body={body}");
        let track_id = body["id"].as_str().expect("track id");
        let (status, detail) = get(boot.app.clone(), &format!("/api/tracks/{track_id}")).await;
        assert_eq!(status, StatusCode::OK, "{key}: detail={detail}");
        let payload = report_card_payload(&detail);
        let (summary, expected_body, expected_tasks) = instantiated_recipe(key);
        assert_eq!(
            expected_tasks.is_empty(),
            expected_task_keys.is_empty(),
            "`{key}`: the recipe parsed to {} task fences but the anchor lists {}",
            expected_tasks.len(),
            expected_task_keys.len()
        );
        let actual_tasks: Vec<Value> = task_blocks(&payload).into_iter().cloned().collect();
        let created_keys: Vec<&str> = actual_tasks
            .iter()
            .map(|task| task["key"].as_str().expect("task key"))
            .collect();
        assert_eq!(
            created_keys,
            expected_task_keys.to_vec(),
            "{key}: the track create instantiated a different recipe than `{key}` names"
        );
        assert_eq!(
            payload.summary, title,
            "{key}: the instantiated report's summary is another template's"
        );
        assert_eq!(payload.summary, summary, "{key}: report summary");
        assert_eq!(actual_tasks, expected_tasks, "{key}: task block payloads");
        assert_eq!(payload.body, expected_body, "{key}: report body");
        for task in &actual_tasks {
            assert_eq!(task["declared_by"], "spec", "{key}: {task}");
            assert_eq!(task["ready"], false, "{key}: {task}");
            assert!(
                task.get("released_by_user").is_none(),
                "{key}: an instantiated task must carry no user release; {task}"
            );
        }
    }

    // Reverse direction: ids absent from the picker are also rejected by create.
    for absent in ["definitely-not-a-template", "issue-development-x"] {
        let (status, body) = post(
            boot.app.clone(),
            "/api/tracks",
            create_body(
                &boot.area_id,
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
                .contains("known track template"),
            "`{absent}`: body={body}"
        );
    }
}
