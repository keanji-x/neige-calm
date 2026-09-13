//! #1635 S5 — operator-provided templates (`--templates-dir`) through the real
//! routes: the picker lists them under `site/<stem>`, `POST /api/tracks`
//! instantiates them, the area default accepts them, and an unknown `site/…`
//! id is a 400 on both write surfaces.
//!
//! The roster reaches `RouteState.templates` through
//! `AppState::with_templates_dir`, which runs the same
//! `TemplateRoster::for_boot` `AppState::boot` runs — so this file exercises
//! the production loader over a real directory, not a fixture roster. The
//! fail-closed cases (bad front matter, `id ≠ stem`, a body that does not
//! compile, …) are the loader's own unit tests in
//! `calm_server::templates::site_dir_tests`; the boot itself — roster before
//! storage, and the hand-over into `AppState::new` — is `main.rs`'s
//! `a_bad_templates_dir_fails_the_boot_before_storage_exists` and
//! `a_templates_dir_reaches_the_picker_through_the_boot`.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
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
use calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR;
use calm_types::report_blocks::{KIND_TASK, render_fence};
use calm_types::report_contract::canonical_line;
use calm_types::track_report::work_brief_header;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::common;
use crate::support::git_helpers::attached_repo_fixture;

const SITE_X: &str = "site/x";
const SITE_X_TITLE: &str = "Operator template X";
const SITE_TASK_KEY: &str = "site-task";
const SITE_TASK_GOAL: &str = "Do the operator's thing.";

/// The one site template this file boots with: canonical work-brief header,
/// a closed contract comment, prose, one canonical `task` fence. Returned as
/// (front matter + body, body) so the create assertion can compare the
/// instantiated report against the body *after* the front matter.
fn site_template_x() -> (String, String) {
    let mut body = canonical_line(&work_brief_header());
    body.push_str("\n<!-- operator contract note: closed -->\n\n# Plan\n\nOperator prose.\n\n");
    body.push_str(&render_fence(
        KIND_TASK,
        &json!({
            "key": SITE_TASK_KEY,
            "kind": "codex",
            "goal": SITE_TASK_GOAL,
            "acceptance": "It is done.",
            "declared_by": PLANNER_DECLARATION_AUTHOR,
            "depends_on": [],
            "no_gate_reason": "site fixture",
            "ready": false
        }),
    ));
    let file = format!("+++\nid = \"x\"\ntitle = \"{SITE_X_TITLE}\"\n+++\n{body}");
    (file, body)
}

struct Boot {
    app: axum::Router,
    area_id: String,
    repo: Arc<dyn Repo>,
    site_body: String,
    _tmp: TempDir,
    _templates_dir: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let templates_dir = TempDir::new().expect("templates tempdir");
    let (file, site_body) = site_template_x();
    std::fs::write(templates_dir.path().join("x.md"), file).expect("write x.md");

    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let area = repo
        .area_create(NewArea {
            name: "site-template-test".into(),
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
            tmp.path().join("plugins-data"),
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
    )
    .with_templates_dir(templates_dir.path());
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
        site_body,
        _tmp: tmp,
        _templates_dir: templates_dir,
    }
}

async fn request(app: axum::Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method(method)
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

fn create_body(area_id: &str, title: &str, template_id: &str) -> Value {
    json!({
        "area_id": area_id,
        "title": title,
        "cwd": attached_repo_fixture(&format!("1635-s5-{title}")),
        "attach_folder": true,
        "theme": {"fg": [216, 219, 226], "bg": [15, 20, 24]},
        "template_id": template_id,
    })
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

/// `GET /api/track-templates` lists the site entry after the builtins, under
/// its `site/<stem>` id, with the file's title and its task projected — and
/// the builtin entries are still there, unchanged in number and order.
#[tokio::test]
async fn picker_lists_the_site_template_after_the_builtins() {
    let boot = boot().await;
    let (status, listing) = get(boot.app.clone(), "/api/track-templates").await;
    assert_eq!(status, StatusCode::OK, "listing={listing}");
    let listed: Vec<&str> = listing
        .as_array()
        .expect("array")
        .iter()
        .map(|t| t["id"].as_str().expect("id"))
        .collect();
    let mut expected: Vec<&str> = calm_server::templates::TemplateRoster::builtin()
        .entries()
        .iter()
        .map(|t| t.key())
        .collect();
    expected.push(SITE_X);
    assert_eq!(
        listed, expected,
        "builtin ids in roster order, then the site id"
    );

    let site = listing
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == SITE_X)
        .expect("site/x listed");
    assert_eq!(site["title"], SITE_X_TITLE);
    assert_eq!(
        site["tasks"],
        json!([{ "key": SITE_TASK_KEY, "goal": SITE_TASK_GOAL }]),
        "the picker projects the site file's own task fence"
    );
    assert!(
        site.get("input_schema").is_none(),
        "no plugin can bind a site/ id, so no input schema: {site}"
    );
}

/// `POST /api/tracks {template_id: "site/x"}` → 201; the track's report is
/// the file's body after the front matter, its summary the file's title, and
/// `tracks.template_id` stores `site/x`.
#[tokio::test]
async fn create_from_a_site_template_instantiates_the_file_body() {
    let boot = boot().await;
    let (status, created) = request(
        boot.app.clone(),
        Method::POST,
        "/api/tracks",
        create_body(&boot.area_id, "from-site-x", SITE_X),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={created}");
    assert_eq!(created["template_id"], SITE_X);
    let track_id = created["id"].as_str().expect("track id").to_string();

    let (status, detail) = get(boot.app.clone(), &format!("/api/tracks/{track_id}")).await;
    assert_eq!(status, StatusCode::OK, "detail={detail}");
    let payload = report_card_payload(&detail);
    assert_eq!(
        payload.summary, SITE_X_TITLE,
        "summary is the front matter title"
    );
    assert_eq!(
        payload.body, boot.site_body,
        "the report body is the file's bytes after the closing `+++` line"
    );
    let task_keys: Vec<&str> = payload
        .blocks
        .as_ref()
        .into_iter()
        .flatten()
        .filter(|block| block.kind == KIND_TASK)
        .map(|block| block.payload["key"].as_str().expect("task key"))
        .collect();
    assert_eq!(task_keys, [SITE_TASK_KEY]);

    let row = boot
        .repo
        .track_get(&track_id)
        .await
        .expect("track_get")
        .expect("created track row");
    assert_eq!(
        row.template_id.as_deref(),
        Some(SITE_X),
        "tracks.template_id carries the site/ key"
    );
}

/// The unprefixed stem is not a template: `x` alone is a 400, exactly like
/// any unknown id, and nothing is written.
#[tokio::test]
async fn the_bare_stem_and_an_unknown_site_id_are_400_on_create() {
    let boot = boot().await;
    for id in ["x", "site/none", "site/", "SITE/X"] {
        let tracks_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks")
            .fetch_one(&boot.repo.sqlite_pool().expect("pool"))
            .await
            .unwrap();
        let (status, body) = request(
            boot.app.clone(),
            Method::POST,
            "/api/tracks",
            create_body(&boot.area_id, &format!("bad-{}", id.replace('/', "-")), id),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{id}: body={body}");
        let error = body["error"].as_str().unwrap_or_default();
        assert!(
            error.contains("`template_id`") && error.contains(id),
            "{id}: the 400 must name the field and the id: {error}"
        );
        let tracks_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks")
            .fetch_one(&boot.repo.sqlite_pool().expect("pool"))
            .await
            .unwrap();
        assert_eq!(tracks_before, tracks_after, "{id}: a 400 mints nothing");
    }
}

/// Areas: `default_template_id: "site/x"` is accepted on create and on patch;
/// `site/none` is a 400 on both.
#[tokio::test]
async fn area_default_template_accepts_site_ids_and_refuses_unknown_ones() {
    let boot = boot().await;
    let (status, area) = request(
        boot.app.clone(),
        Method::POST,
        "/api/areas",
        json!({ "name": "Ops", "color": "#123456", "default_template_id": SITE_X }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{area}");
    assert_eq!(area["default_template_id"], SITE_X);
    let area_id = area["id"].as_str().expect("area id").to_string();
    let stored = boot
        .repo
        .area_get(&area_id)
        .await
        .expect("area_get")
        .expect("area row");
    assert_eq!(stored.default_template_id.as_deref(), Some(SITE_X));

    let (status, body) = request(
        boot.app.clone(),
        Method::PATCH,
        &format!("/api/areas/{area_id}"),
        json!({ "default_template_id": "site/none" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("site/none"),
        "{body}"
    );
    let stored = boot
        .repo
        .area_get(&area_id)
        .await
        .expect("area_get")
        .expect("area row");
    assert_eq!(
        stored.default_template_id.as_deref(),
        Some(SITE_X),
        "a refused patch leaves the stored default alone"
    );

    let (status, body) = request(
        boot.app.clone(),
        Method::POST,
        "/api/areas",
        json!({ "name": "Ops 2", "color": "#123456", "default_template_id": "site/none" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}
