//! What `template_id` does on `POST /api/waves`.
//!
//! A matching `template_id` initializes the new wave's report from a **Rust
//! constant recipe** (`calm_server::templates`) inside the create transaction:
//! no hidden wave is minted, no overlay is written, and nothing is read from
//! the database to find the content. An explicit `fork_report_from` still wins
//! over `template_id`, and an unknown `template_id` is a 400 decided before any
//! write.
//!
//! #1110 S6 wrote this file against the opposite implementation — three seeded
//! system-cove template waves, discovered through a `template_key` overlay and
//! forked on create, hidden from lists but returned by detail. #1300 S2 deleted
//! all of it (that seeding was the last production writer signing a report as
//! `EditAuthor::User`). Cases that asserted the seeding are inverted rather
//! than dropped, so the removal itself stays asserted; each one says so in its
//! own doc comment.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{EditAuthor, EventBus};
use calm_server::ids::ActorId;
use calm_server::model::{NewCove, NewOverlay, NewPlugin};
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, DaemonClient};
use calm_server::validation::{
    OVERLAY_TEMPLATE_ENTITY_KIND, OVERLAY_TEMPLATE_KIND, OVERLAY_TEMPLATE_PLUGIN_ID,
    OVERLAY_TEMPLATE_SCHEMA_VERSION,
};
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
// #1300 — a hand-copied `TEMPLATE_KEYS` array lived here, as the expected
// roster for the seeding assertions. It is gone with them, and deliberately not
// replaced: `calm_server::templates::TEMPLATES` is the roster, and a second
// copy in a test file is the drift #1209 spent a slice removing from
// production. Cases that need the whole roster iterate the real one.

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
/// at silently stops covering whatever is added next. `template_key_overlays`
/// below
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

/// Every overlay carrying a `template_key`, as `(key, wave_id)`.
///
/// #1300 — before S2 this measured "which of the three hidden template waves
/// has been seeded", and the tests below asserted it was non-empty. It is kept,
/// renamed, for the opposite job: nothing in the kernel writes a `template_key`
/// any more (`template_overlay_payload_with_key` is deleted), so this is how
/// "creating from a template mints no hidden wave" is *observed* rather than
/// assumed. A removal with no assertion that it happened is not a removal
/// anyone can keep.
async fn template_key_overlays(repo: &Arc<dyn Repo>) -> Vec<(String, String)> {
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

/// #1300 S2 — creating from a template mints **no** hidden wave.
///
/// ## Why this replaced a test of the opposite property
///
/// This case used to be `matching_template_id_seeds_one_wave_per_template_key`:
/// it asserted the three hidden system-cove template waves *appeared*, that
/// they stayed out of every wave list, and that a second create did not
/// duplicate them. All three were properties of the seeding this slice deletes.
///
/// Deleting them and stopping there would have left the removal unasserted:
/// the old seeding could have survived in any form — an unused code path still
/// minting rows, a later change reintroducing it — and every remaining test
/// would still be green, because the rest of the suite only ever looks at the
/// wave the caller asked for. So the case is inverted rather than dropped.
///
/// ## Both success paths, because they used to differ
///
/// `template_id` alone is the obvious one. `template_id` **plus** an explicit
/// `fork_report_from` is the one worth naming: before #1300 the route seeded
/// unconditionally on admission and only *then* checked whether an explicit
/// fork had already claimed the report source (`waves.rs`, the
/// `if fork_report_from.is_none()` after `ensure_templates`), so that
/// combination minted three waves it did not use. Only checking the plain path
/// would leave that one covered by nothing.
///
/// `explicit_fork_report_from_is_not_overwritten` remains the test of *which*
/// report wins; this is the test of what the losing branch costs.
#[tokio::test]
async fn creating_from_a_template_mints_no_hidden_wave() {
    let boot = boot().await;

    // A fork source in the user's own cove, so the second leg can pass an
    // explicit `fork_report_from` alongside a `template_id`.
    let (status, source) = post(
        boot.app.clone(),
        "/api/waves",
        create_body(&boot.cove_id, "fork-source", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "source={source}");
    let source_id = source["id"].as_str().expect("source wave id").to_string();

    for (leg, extra) in [
        ("template only", json!({ "template_id": ISSUE_DEVELOPMENT })),
        (
            "template plus an explicit fork",
            json!({ "template_id": ISSUE_DEVELOPMENT, "fork_report_from": source_id }),
        ),
    ] {
        let waves_before = boot
            .repo
            .waves_window(None, None, None)
            .await
            .unwrap()
            .len();

        let (status, body) = post(
            boot.app.clone(),
            "/api/waves",
            create_body(&boot.cove_id, leg, extra),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{leg}: body={body}");
        let created = body["id"].as_str().expect("wave id").to_string();

        // Exactly one new wave, and it is the one the caller asked for. A
        // count alone would pass if the create minted one hidden wave and
        // failed to mint the requested one.
        let after = boot.repo.waves_window(None, None, None).await.unwrap();
        assert_eq!(
            after.len(),
            waves_before + 1,
            "{leg}: created {} waves, not 1",
            after.len() - waves_before
        );
        assert!(
            after.iter().any(|wave| wave.id.as_str() == created),
            "{leg}: the requested wave is not among them"
        );

        assert!(
            template_key_overlays(&boot.repo).await.is_empty(),
            "{leg}: a `template_key` overlay was minted; the kernel has no writer for one"
        );
        assert!(
            boot.repo.cove_get_system().await.unwrap().is_none(),
            "{leg}: creating from a template must not mint the system cove either"
        );
    }
}

/// Two waves from one template are independent documents with the same content.
///
/// The old suite got this for free from the seeding: both forked the same
/// hidden wave, so "the same content" was true by construction and "independent"
/// was the interesting half. With the hidden wave gone both halves are claims
/// about the new path, and neither is implied by the other — a shared mutable
/// snapshot would satisfy the content check, and two independently *wrong*
/// documents would satisfy the independence check.
///
/// ## Why the write leg exists
///
/// Reading both documents once and finding them equal is not independence; it
/// is the *identical* half stated twice. The escape construction: make a report
/// write fan out to every wave carrying the same `template_id` (one extra
/// `UPDATE ... WHERE template_id = ...` after `card_update_with_crdt_tx` in
/// `wave_report::persist_report`). The two documents are then genuinely one
/// document behind two ids, and a create-time-only comparison stays green. So
/// this case edits one wave and re-reads the other; independence is only
/// asserted about a state the two could actually disagree in.
///
/// **Only `summary` is edited, deliberately.** `guard_non_prose_stomp` refuses
/// a prose-channel write that alters a non-prose block, and the recipe body is
/// almost entirely `task` fences — rewriting it here would fail on the guard
/// rather than on independence, and the repair would be to weaken the case.
/// The summary travels the same `persist_report` write and the same card
/// payload, so it discriminates the same fan-out.
#[tokio::test]
async fn two_waves_from_one_template_are_independent_and_identical() {
    let boot = boot().await;
    let mut reports = Vec::new();
    for leg in ["first", "second"] {
        let (status, body) = post(
            boot.app.clone(),
            "/api/waves",
            create_body(&boot.cove_id, leg, json!({ "template_id": SMALL_CHANGE })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{leg}: body={body}");
        let wave_id = body["id"].as_str().expect("wave id").to_string();
        let (_, detail) = get(boot.app.clone(), &format!("/api/waves/{wave_id}")).await;
        reports.push((wave_id, report_card_payload(&detail)));
    }
    let [(first_id, first), (second_id, second)] = <[_; 2]>::try_from(reports).ok().unwrap();

    assert_ne!(first_id, second_id, "two creates must be two waves");
    assert_eq!(first.summary, second.summary);
    assert_eq!(first.body, second.body);

    // ---- independence: edit one, re-read the other ----
    const EDITED: &str = "first wave's own summary";
    assert_ne!(
        first.summary, EDITED,
        "the edit must change something, or the re-read below asserts nothing"
    );
    let (source_wave, report_card, current) =
        resolve_report_for_wave(boot.repo.as_ref(), &first_id)
            .await
            .expect("first wave's report");
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
        // Same body, new summary — see the doc comment on `guard_non_prose_stomp`.
        WaveReportPayload::new(EDITED, first.body.clone()),
        if_doc_rev,
        None,
        None,
        false,
    )
    .await
    .expect("edit the first wave's report");

    let (status, first_detail) = get(boot.app.clone(), &format!("/api/waves/{first_id}")).await;
    assert_eq!(status, StatusCode::OK, "detail={first_detail}");
    assert_eq!(
        report_card_payload(&first_detail).summary,
        EDITED,
        "the edit did not land, so the re-read below proves nothing"
    );

    let (status, second_detail) = get(boot.app.clone(), &format!("/api/waves/{second_id}")).await;
    assert_eq!(status, StatusCode::OK, "detail={second_detail}");
    let second_after = report_card_payload(&second_detail);
    assert_eq!(
        second_after.summary, second.summary,
        "editing one template-created wave changed the other's summary: the two \
         waves share a document"
    );
    assert_eq!(
        second_after.body, second.body,
        "editing one template-created wave changed the other's body: the two \
         waves share a document"
    );
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

/// A forged `template_key` in the user's own cove cannot influence what
/// `template_id` produces.
///
/// ## What this used to test, and why the property had to be restated
///
/// Before #1300 the create path resolved `template_id` by *searching the
/// database* for a system-cove wave carrying a matching `template_key` overlay.
/// That search is an attack surface: this case forges the overlay on a wave in
/// the user's own cove, stamps a recognizable plan into its report, and
/// requires the lookup to reject it on the cove check.
///
/// #1300 deletes the lookup — a template is a Rust constant, so there is no
/// query to poison and no cove check to get wrong. Left as written the case
/// would be **vacuously green**: the forged wave cannot lose a race that no
/// longer happens.
///
/// It is restated rather than deleted, because the property is not "the cove
/// check works", it is *where the content comes from*. So the same forgery is
/// set up, and the assertion becomes: the created report is the recipe, byte
/// for byte, and carries nothing from the forged wave. That stays falsifiable —
/// reintroducing any database lookup for template content turns it red — while
/// the old form would have stopped being able to fail.
///
/// (The last assertion of the old version was `kernel seed must still mint a
/// system-cove issue-development`. That one did not go vacuous, it went red,
/// which is how this case surfaced: it was two properties in one test, and only
/// one of them was about the attack.)
#[tokio::test]
async fn a_forged_template_key_cannot_influence_what_a_template_creates() {
    let boot = boot().await;

    // The forgery, exactly as before: an `as_template` wave in the *user's*
    // cove, wearing the `issue-development` key, holding recognizable content.
    let (status, stolen) = post(
        boot.app.clone(),
        "/api/waves",
        create_body(
            &boot.cove_id,
            "forged-template",
            json!({ "as_template": true }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={stolen}");
    let stolen_id = stolen["id"].as_str().unwrap().to_string();
    // #1297 closed the front door on this forge: `POST /api/overlays` now
    // refuses the reserved `kernel` / `view` namespaces outright. Assert that
    // first — it is the cheap layer, and it is the one a client can reach.
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

    // Then plant the stolen key anyway, through the kernel-internal writer,
    // so the deeper invariant this test exists for still gets exercised: even
    // a row that *did* land — via a future internal bug, a restored backup, or
    // a row predating #1297 — must not hijack the auto-fork lookup, because
    // that lookup also requires the wave to live in the system cove.
    boot.repo
        .overlay_upsert(NewOverlay {
            plugin_id: OVERLAY_TEMPLATE_PLUGIN_ID.into(),
            entity_kind: OVERLAY_TEMPLATE_ENTITY_KIND.into(),
            entity_id: stolen_id.clone(),
            kind: OVERLAY_TEMPLATE_KIND.into(),
            // Spelled out rather than built by a helper: #1300 deleted
            // `template_overlay_payload_with_key` along with the kernel's last
            // writer of `template_key`, and reviving it as a test-only
            // constructor would put the forged shape back in production code.
            payload: json!({
                "schemaVersion": OVERLAY_TEMPLATE_SCHEMA_VERSION,
                "template_key": ISSUE_DEVELOPMENT,
            }),
        })
        .await
        .expect("plant stolen template_key");
    let (stolen_wave, report_card, current) =
        resolve_report_for_wave(boot.repo.as_ref(), &stolen_id)
            .await
            .expect("forged report");
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
        WaveReportPayload::new("forged template", "# Forged\n\nforged-user-cove-plan\n"),
        if_doc_rev,
        None,
        None,
        false,
    )
    .await
    .expect("stamp forged report");

    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        create_body(
            &boot.cove_id,
            "after-forged-key",
            json!({ "template_id": ISSUE_DEVELOPMENT }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let wave_id = body["id"].as_str().expect("wave id");
    let (status, detail) = get(boot.app.clone(), &format!("/api/waves/{wave_id}")).await;
    assert_eq!(status, StatusCode::OK, "detail={detail}");
    let payload = report_card_payload(&detail);

    // The positive form, not `!contains("forged-user-cove-plan")`: an
    // implementation that produced an *empty* report would satisfy the negative
    // and fail this.
    let (summary, expected_body, _) = instantiated_recipe(ISSUE_DEVELOPMENT);
    assert_eq!(payload.summary, summary);
    assert_eq!(
        payload.body, expected_body,
        "a forged template_key must not reach the created report"
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
        template_key_overlays(&boot.repo).await.is_empty(),
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

/// A read of the picker returns the whole constant roster and writes nothing.
///
/// The snapshot comparison is the load-bearing half: returning the right
/// constants proves nothing on its own, because the deleted seeding path would
/// also have returned the right values. What must hold is that the read left
/// the database as it found it.
///
/// The roster half used to be one title and one task key of `small-change`,
/// which the name over-sold: a response hard-coded to "the correct
/// `small-change`, and nothing else" satisfied it. It now asserts the exact set
/// of ids the roster declares, in the roster's order, and that no fourth entry
/// rode along. *Which* recipe each id names is
/// `each_template_key_names_its_own_recipe`'s job, not this one's — this case
/// owns "the listing is the roster and the read is read-only".
///
/// #1300 — this absorbed `listing_wave_templates_does_not_materialize_seed_state`,
/// which asserted the same "a GET writes nothing" property across an unseeded
/// and a seeded database. Its second state cannot be built any more (there is
/// nothing to seed), so it went red rather than vacuous; the two-state shape
/// survives here as "empty" and "after a create", which is the distinction that
/// still exists.
#[tokio::test]
async fn listing_templates_returns_constants_and_writes_nothing() {
    let boot = boot().await;

    // Two states, because a read can only be shown not to write by comparing
    // the database around it, and "before anything exists" is the easy half.
    // The second leg runs after a real create, so the read is exercised against
    // a populated database — which is where the deleted lazy seed used to fire.
    for leg in ["empty database", "after a create"] {
        let before = db_snapshot(&boot.repo).await;
        let (status, body) = get(boot.app.clone(), "/api/wave-templates").await;
        assert_eq!(status, StatusCode::OK, "{leg}: body={body}");
        let listed_ids: Vec<&str> = body
            .as_array()
            .expect("array body")
            .iter()
            .map(|entry| entry["id"].as_str().expect("template id"))
            .collect();
        let roster_ids: Vec<&str> = calm_server::templates::TEMPLATES
            .iter()
            .map(|template| template.key)
            .collect();
        assert_eq!(
            listed_ids, roster_ids,
            "{leg}: the listing is the roster, in the roster's order"
        );
        // Every entry is a usable picker row. A roster id that came back with
        // no title or no tasks is drift the id set alone cannot see.
        for entry in body.as_array().expect("array body") {
            assert!(
                entry["title"].as_str().is_some_and(|t| !t.is_empty()),
                "{leg}: {entry} has no title"
            );
            assert!(
                !task_keys(entry).is_empty(),
                "{leg}: {entry} advertises no tasks"
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
                "/api/waves",
                create_body(
                    &boot.cove_id,
                    "populate",
                    json!({ "template_id": SMALL_CHANGE }),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "body={created}");
        }
    }
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

// ---------------------------------------------------------------------------
// #1300 S2 — what "create a wave from a template" produces, derived not
// transcribed.
//
// This is the characterization test the seeding removal is measured against.
// It is written and verified green **against the old seeding path first**, and
// must stay green after the implementation is replaced. That order is the whole
// of the equivalence evidence: a test written after the switch can only say the
// new code agrees with itself.
// ---------------------------------------------------------------------------

/// The report a template must instantiate to, derived from the recipe.
///
/// ## Why derived and not written out
///
/// Spelling the expected task payloads into this file would make the test a
/// **change detector**: rewording one template goal would turn it red with no
/// defect, and the fix would be to paste the new text — which teaches everyone
/// that red here means "go update the expectation". Deriving keeps the two
/// sides moving together for a content edit and apart for an implementation
/// drift, which is the only difference this test exists to see.
///
/// It is still an oracle and not a tautology, because the two sides travel
/// different roads: this walks the recipe's slices in the test process, while
/// the value it is compared against came back over HTTP from a wave the server
/// created.
///
/// ## The one normalization, stated once
///
/// A template's task blocks are instantiated as `declared_by: "spec"` and
/// `ready: false` — nothing in a recipe was decided for *this* wave. Everything
/// else about a block is carried verbatim.
///
/// ## Why every slice, not just the task fences
///
/// Concatenating only the normalized task fences would silently drop the
/// maintenance-contract prefix (#1185 §1.5 B), the `# Plan` intro, and the
/// newline-only prose slices that `report_from_tasks` leaves between fences —
/// and an implementation that lost all of them would still pass. Non-task
/// slices are therefore carried through byte for byte.
fn instantiated_recipe(key: &str) -> (String, String, Vec<Value>) {
    use calm_types::report_blocks::{KIND_TASK, parse_fence, render_fence, split_body};

    let recipe = calm_server::templates::template_report(key)
        .unwrap_or_else(|| panic!("`{key}` is not a known template"));
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
    assert!(
        !tasks.is_empty(),
        "`{key}`: the recipe parsed to no task fences, so this test would assert nothing"
    );
    (recipe.summary, body, tasks)
}

/// Creating a wave from each template produces exactly that template's recipe,
/// normalized once.
///
/// ## What is deliberately NOT asserted, and why
///
/// **`blocks[].id` and `blocks[].rev`.** Both are per-wave bookkeeping, and
/// neither is a cross-implementation contract. `rev` in particular differs by
/// construction between the two implementations this test spans: the seeding
/// path writes the recipe *over* the default report skeleton, so the blocks the
/// aligner matches come out at `rev: 2`, while a wave initialized straight from
/// the recipe starts every block at `rev: 1`. A fresh wave whose blocks all
/// start at 1 is self-consistent, and the readers of `rev` — the block-level
/// CAS anchors used by the MCP and REST block writers — are all within one
/// wave. Asserting equality here would make this test red at the moment of the
/// switch for a difference nobody can observe, and the repair would be to
/// weaken it, with the reason nowhere on record.
///
/// **The CRDT bytes**, for the same reason one layer down.
///
/// ## What this cannot see
///
/// The fork implementation also rewrites `neige://wave/...` links, normalizes
/// tombstones, and strips `released_by_user`. None of the three built-in
/// recipes contains any of that vocabulary, so those branches take no input
/// here and this test says nothing about them. That is a fact about today's
/// three recipes, not about templates: a recipe that grew a tombstone would
/// need its own case.
#[tokio::test]
async fn creating_from_a_template_instantiates_its_recipe() {
    let boot = boot().await;
    for template in &calm_server::templates::TEMPLATES {
        let key = template.key;
        let (status, body) = post(
            boot.app.clone(),
            "/api/waves",
            create_body(&boot.cove_id, key, json!({ "template_id": key })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{key}: body={body}");
        let wave_id = body["id"].as_str().expect("wave id");

        let (status, detail) = get(boot.app.clone(), &format!("/api/waves/{wave_id}")).await;
        assert_eq!(status, StatusCode::OK, "{key}: detail={detail}");
        let payload = report_card_payload(&detail);

        let (summary, expected_body, expected_tasks) = instantiated_recipe(key);
        assert_eq!(payload.summary, summary, "{key}: report summary");

        // Ordered task payloads FIRST, then the flat body. The body compare
        // subsumes this one — every difference a fence can carry shows up in
        // it — but it reports as a single multi-kilobyte string diff, and
        // these recipes are kilobytes of prose contract. Asserting the parsed
        // payloads first means the common failure (a field of one task) names
        // that task and that field. Measured, not assumed: dropping the
        // `declared_by` normalization printed the whole document twice until
        // this order was fixed.
        let actual_tasks: Vec<Value> = task_blocks(&payload).into_iter().cloned().collect();
        assert_eq!(actual_tasks, expected_tasks, "{key}: task block payloads");

        // The body still has to be compared: the payload list above says
        // nothing about the contract prefix, the intro, the order the fences
        // appear in, or the whitespace between them.
        assert_eq!(payload.body, expected_body, "{key}: report body");

        // Whole-payload equality above already covers these; they are spelled
        // out because they are the three the removal could plausibly get wrong,
        // and a named assertion says which one broke.
        for task in &actual_tasks {
            assert_eq!(task["declared_by"], "spec", "{key}: {task}");
            assert_eq!(task["ready"], false, "{key}: {task}");
            assert!(
                task.get("released_by_user").is_none(),
                "{key}: an instantiated task must carry no user release; {task}"
            );
        }
    }
}

/// The hand-written half: **which** recipe each key names.
///
/// ## Why a derived oracle needs this, stated as what each side can and cannot
/// see
///
/// `creating_from_a_template_instantiates_its_recipe` derives its expectation
/// from `templates::template_report`, which is the production `key → recipe`
/// match itself. That is the right call for *content* — it is what stops the
/// case being a change detector over kilobytes of prose — but it means the two
/// sides of that comparison share the mapping, so a class of drift moves both
/// and stays green:
///
///   * swapping two arms of the `template_report` match (`SMALL_CHANGE` returns
///     `investigation_report()`);
///   * a recipe rewritten wholesale into a different workflow;
///   * a `TEMPLATES` entry whose `title` no longer describes its `key`.
///
/// This case is the anchor for exactly that class and nothing else. It pins,
/// per key, only the two facts that identify *which* recipe answered:
///
///   * the roster title, and
///   * the ordered task keys.
///
/// ## What is deliberately NOT pinned here
///
/// Goals, acceptance criteria, `context`, `depends_on`, `no_gate_reason`, the
/// intro prose, the contract prefix — none of it. Every one of those is
/// content, all of it is already compared byte-for-byte by the derived oracle,
/// and copying any of it here would rebuild the change detector this file's
/// design note rejects: reword one goal and a maintainer would have two places
/// to paste it into, which is how "red here means update the expectation" gets
/// taught. Task keys and titles are the cheapest values that are *identities*
/// rather than prose — a template's task keys are also what the picker and the
/// plan projection address blocks by, so they do not churn on an edit.
///
/// ## Both roads, because the mapping is read twice
///
/// `GET /api/wave-templates` reads the mapping to render the picker, and
/// `POST /api/waves` reads it to instantiate. A swap in only one of them is a
/// real defect (the picker advertises one plan, create produces another), so
/// both are asserted against the same hand-written row.
#[tokio::test]
async fn each_template_key_names_its_own_recipe() {
    // key, roster title, ordered task keys. Hand-written on purpose — this is
    // the one table in this file that must NOT be derived from production.
    let anchors: [(&str, &str, &[&str]); 3] = [
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
    ];
    assert_eq!(
        anchors.len(),
        calm_server::templates::TEMPLATES.len(),
        "the roster grew or shrank; this table is the one place that must be \
         edited by hand when it does"
    );

    let boot = boot().await;
    let (status, listing) = get(boot.app.clone(), "/api/wave-templates").await;
    assert_eq!(status, StatusCode::OK, "listing={listing}");

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
            "/api/waves",
            create_body(&boot.cove_id, key, json!({ "template_id": key })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{key}: body={body}");
        let wave_id = body["id"].as_str().expect("wave id");
        let (status, detail) = get(boot.app.clone(), &format!("/api/waves/{wave_id}")).await;
        assert_eq!(status, StatusCode::OK, "{key}: detail={detail}");
        let payload = report_card_payload(&detail);
        let created_keys: Vec<&str> = task_blocks(&payload)
            .iter()
            .map(|task| task["key"].as_str().expect("task key"))
            .collect();
        assert_eq!(
            created_keys,
            expected_task_keys.to_vec(),
            "{key}: the wave create instantiated a different recipe than `{key}` names"
        );
        assert_eq!(
            payload.summary, title,
            "{key}: the instantiated report's summary is another template's"
        );
    }
}
