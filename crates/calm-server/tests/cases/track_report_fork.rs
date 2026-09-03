#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{self, AuthConfig, AuthState, SESSION_COOKIE};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_insert_tx, session_mark_track_root_tx};
use calm_server::error::CalmError;
use calm_server::event::{EditAuthor, EventBus};
use calm_server::ids::{ActorId, CardId, TrackId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::track_report_blocks::{
    TOOL_REPORT_BLOCKS_DELETE, TOOL_REPORT_BLOCKS_UPSERT,
};
use calm_server::mcp_server::tools::track_state::TOOL_TRACK_STATE;
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack};
use calm_server::operation::planner_harness_start_adapter::render_planner_developer_instructions_for_test;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::session_projection_repo::AgentProvider;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_report::{TrackReportPayload, persist_report};
use calm_types::report_blocks::render_fence;
use calm_types::worker::{
    LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession, WorkerSessionId,
    WorkerSessionState,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::support::git_helpers::attached_repo_fixture;

/// #1147 S3 — `POST /api/tracks` now validates an attached `cwd`: absolute,
/// existing, inside a Git work tree. These fork fixtures only ever needed *a*
/// target directory (they assert on report/CRDT rows, never on the path), so
/// each named variant becomes a real, shared, idempotent Git work tree.
/// The one spelling of an internal block reference these tests build.
///
/// Extracted so the fixture that plants the reference and the assertion that
/// checks it after the fork cannot drift apart, and so the pair costs one
/// occurrence of the retiring URI scheme instead of two. #1316 has not renamed
/// the scheme yet, so each literal here is one more site that rename will have
/// to find.
fn internal_block_ref(track_id: &str, block_id: &str) -> String {
    format!("neige://wave/{track_id}#{block_id}")
}

fn target_cwd(suffix: &str) -> String {
    attached_repo_fixture(&format!("track-report-fork-target{suffix}"))
}

struct Boot {
    app: axum::Router,
    state: AppState,
    repo: Arc<dyn Repo>,
    area_id: String,
    other_area_id: String,
    source_track_id: String,
    source_report_id: String,
    cookie: String,
    _tmp: TempDir,
}

fn long_fixture_text() -> String {
    "long-fixture-segment-".repeat(450)
}

fn entity_encode_first(value: &str) -> String {
    let first = value.chars().next().unwrap();
    format!("&#{};{}", u32::from(first), &value[first.len_utf8()..])
}

fn fixture_prose(source_track_id: &str, internal_block_id: &str) -> String {
    format!(
        concat!(
            "# Fixture\n\n{long}\n\n",
            "[inline](neige://wave/{0}#{1})\n",
            "[second-inline](neige://wave/{0}#{1})\n",
            "[reference][same]\n",
            "<neige://wave/{0}#{1}>\n",
            "[dangling](neige://wave/{0}#b_dead)\n",
            "`[code](neige://wave/{0}#b_dead)`\n",
            "```markdown\n[fenced](neige://wave/{0}#b_dead)\n```\n",
            "[external](neige://wave/external-track#b_4444)\n",
            "\n[same]: neige://wave/{0}#{1}\n",
        ),
        source_track_id,
        internal_block_id,
        long = long_fixture_text(),
    )
}

fn primary_task_payload(source_track_id: &str, internal_block_id: &str) -> Value {
    json!({
        "key": "build",
        "kind": "codex",
        "goal": format!("Goal [internal](neige://wave/{source_track_id}#{internal_block_id})"),
        "acceptance": format!("Accept <neige://wave/{source_track_id}#{internal_block_id}>") ,
        "refs": [format!("neige://wave/{source_track_id}#{internal_block_id}")],
        "ready": true,
        "declared_by": "user"
    })
}

fn seed_fixture_body() -> String {
    let prose = format!("# Fixture\n\n{}\n\ncapture pending\n", long_fixture_text());
    let chart = json!({
        "symbol": "985.TEST",
        "period": "day",
        "candles": [[1719800000000_i64, 10, 12, 9, 11, 100], [1719886400000_i64, 11, 13, 10, 12, 120]],
        "overlays": ["ma20", "ma60"],
        "caption": "fixture chart"
    });
    let table = json!({
        "columns": [
            {"key": "name", "label": "Name", "align": "left"},
            {"key": "value", "label": "Value", "align": "right"}
        ],
        "rows": [{"name": "alpha", "value": 1}, {"name": "beta", "value": null}],
        "caption": "fixture table",
        "highlight": "alpha"
    });
    let app = json!({"src": "/apps/fork-fixture?mode=deep", "title": "Fixture app", "height": 640});
    let primary_task = json!({
        "key": "build",
        "kind": "codex",
        "goal": "Goal pending source REST id capture",
        "acceptance": "Acceptance pending source REST id capture",
        "refs": [],
        "ready": true,
        "declared_by": "user"
    });
    let duplicate_task = json!({
        "key": "build",
        "kind": "terminal",
        "goal": "Second declaration with the same key",
        "acceptance": "Second declaration remains exact",
        "refs": [],
        "ready": true,
        "declared_by": "user"
    });
    let tombstone = json!({
        "key": "rejected",
        "tombstone": { "reason": "not now" },
        "declared_by": "user",
        "tombstoned_by": "user"
    });
    format!(
        "{prose}{}# Anchor\n\nanchor payload\n{}{}{}{}{}",
        render_fence("chart.candles", &chart),
        render_fence("table", &table),
        render_fence("app", &app),
        render_fence("task", &primary_task),
        render_fence("task", &duplicate_task),
        render_fence("task", &tombstone),
    )
}

async fn boot() -> Boot {
    let tmp = TempDir::new().unwrap();
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "fork-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let source = repo
        .track_create(NewTrack {
            area_id: area.id.clone(),
            title: "source".into(),
            sort: None,
            cwd: tmp.path().to_string_lossy().into_owned(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let other_area = repo
        .area_create(NewArea {
            name: "fork-test-other".into(),
            color: "#111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let report = repo
        .card_create(NewCard {
            track_id: source.id.clone(),
            kind: "track-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
            title: None,
        })
        .await
        .unwrap();

    let events = EventBus::new();
    let card_roles = CardRoleCache::new();
    let track_areas = calm_server::track_area_cache::TrackAreaCache::new();
    repo.seed_track_area_cache(&track_areas).await.unwrap();
    let write = calm_server::state::WriteContext::new(card_roles.clone(), track_areas.clone());
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient {
            data_dir: tmp.path().to_path_buf(),
            proc_supervisor_sock: None,
        }),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            tmp.path().join("plugins"),
            Vec::new(),
            EventBus::new(),
            write,
        )),
        {
            let mut codex = CodexClient::new_stub();
            codex.codex_bin = "/nonexistent-codex-bin-track-report-fork".into();
            Arc::new(codex)
        },
        Some(card_roles),
        Some(track_areas),
    );
    let auth_state = AuthState::new(AuthConfig {
        username: Some("alice".into()),
        password: Some("hunter2".into()),
        dev_autologin: false,
        display_name: "alice".into(),
    });
    let protected = routes::protected_router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            auth_state.clone(),
            auth::require_session,
        ));
    let app = axum::Router::new()
        .merge(protected)
        .merge(routes::public_router())
        .with_state(state.clone())
        .merge(auth::router().with_state(auth_state));

    let source_id = source.id.to_string();
    let cookie = login(&app).await;
    persist_report(
        repo.as_ref(),
        &state.events,
        state.write(),
        ActorId::User,
        EditAuthor::User,
        source.clone(),
        report.clone(),
        TrackReportPayload::initial(),
        TrackReportPayload::new("fork source summary", seed_fixture_body()),
        0,
        None,
        None,
        false,
    )
    .await
    .unwrap();
    let (status, seeded_report) = request_json(
        &app,
        "GET",
        format!("/api/tracks/{source_id}/report"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let seeded_blocks = seeded_report["blocks"].as_array().unwrap();
    let internal_block_id = seeded_blocks
        .iter()
        .filter(|block| block["kind"] == "prose")
        .nth(1)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let prose_block = seeded_blocks
        .iter()
        .find(|block| block["kind"] == "prose")
        .unwrap();
    let (status, response) = request_json(
        &app,
        "PATCH",
        format!(
            "/api/tracks/{source_id}/report/blocks/{}",
            prose_block["id"].as_str().unwrap()
        ),
        &cookie,
        Some(json!({
            "kind": "prose",
            "markdown": fixture_prose(&source_id, &internal_block_id),
            "ifBlockRev": prose_block["rev"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "update fixture prose: {response}");
    let task_block = seeded_blocks
        .iter()
        .find(|block| block["kind"] == "task" && block["payload"]["key"] == "build")
        .unwrap();
    let (status, response) = request_json(
        &app,
        "PATCH",
        format!(
            "/api/tracks/{source_id}/report/blocks/{}",
            task_block["id"].as_str().unwrap()
        ),
        &cookie,
        Some(json!({
            "kind": "task",
            "payload": primary_task_payload(&source_id, &internal_block_id),
            "ifBlockRev": task_block["rev"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "update fixture task: {response}");
    Boot {
        app,
        state,
        repo,
        area_id: area.id.to_string(),
        other_area_id: other_area.id.to_string(),
        source_track_id: source_id,
        source_report_id: report.id.to_string(),
        cookie,
        _tmp: tmp,
    }
}

#[tokio::test]
async fn fork_source_and_area_checks_fail_with_four_hundred_and_roll_back_creation() {
    let boot = boot().await;
    let (status, body) = request_json(
        &boot.app,
        "POST",
        "/api/tracks".into(),
        &boot.cookie,
        Some(json!({
            "area_id": boot.other_area_id,
            "title": "cross-area target",
            "sort": null,
            "cwd": target_cwd("-cross"),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_track_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body = {body}");
    assert!(
        boot.repo
            .tracks_by_area(&boot.other_area_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        boot.repo
            .area_folders_by_area(&boot.other_area_id)
            .await
            .unwrap()
            .is_empty()
    );

    let source_area_id = boot.area_id.clone();
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            sqlx::query("UPDATE areas SET kind='system' WHERE id=?1")
                .bind(source_area_id)
                .execute(&mut **tx)
                .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    let (status, body) = request_json(
        &boot.app,
        "POST",
        "/api/tracks".into(),
        &boot.cookie,
        Some(json!({
            "area_id": boot.other_area_id,
            "title": "system-source target",
            "sort": null,
            "cwd": target_cwd("-system"),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_track_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "system source body = {body}");
    assert_eq!(
        boot.repo
            .tracks_by_area(&boot.other_area_id)
            .await
            .unwrap()
            .len(),
        1,
        "a system-area source may seed a track in another area"
    );

    let tracks_before = boot.repo.tracks_by_area(&boot.area_id).await.unwrap().len();
    let (status, body) = request_json(
        &boot.app,
        "POST",
        "/api/tracks".into(),
        &boot.cookie,
        Some(json!({
            "area_id": boot.area_id,
            "title": "missing-source target",
            "sort": null,
            "cwd": target_cwd("-missing"),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": "track-does-not-exist",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body = {body}");
    assert_eq!(
        boot.repo.tracks_by_area(&boot.area_id).await.unwrap().len(),
        tracks_before,
        "failed fork must roll back the new track row"
    );
}

async fn login(app: &axum::Router) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"username":"alice", "password":"hunter2"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let raw = response.headers()[header::SET_COOKIE].to_str().unwrap();
    let cookie = raw.split(';').next().unwrap().to_string();
    assert!(cookie.starts_with(&format!("{SESSION_COOKIE}=")));
    cookie
}

async fn request_json(
    app: &axum::Router,
    method: &str,
    uri: String,
    cookie: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    request_json_as(app, method, uri, cookie, body, None).await
}

async fn request_json_as(
    app: &axum::Router,
    method: &str,
    uri: String,
    cookie: &str,
    body: Option<Value>,
    actor: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, cookie);
    if let Some(actor) = actor {
        request = request.header("X-Calm-Actor", actor);
    }
    let body = if let Some(body) = body {
        request = request.header("content-type", "application/json");
        Body::from(body.to_string())
    } else {
        Body::empty()
    };
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

fn block_index(report: &Value) -> Vec<(String, u64)> {
    report["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|block| {
            (
                block["id"].as_str().unwrap().to_string(),
                block["rev"].as_u64().unwrap(),
            )
        })
        .collect()
}

/// Counts every table the fork persistence path writes, so a "zero residue"
/// assertion covers the whole surface: the track row, the attached area folder,
/// both cards (planner + reportcard), the overlays, and the `tasks` rows that
/// `track_report::write::structural_init_report_tx` projects out of the copied
/// report.
async fn fork_row_counts(repo: &dyn Repo) -> (i64, i64, i64, i64, i64, i64) {
    calm_server::db::write_in_tx_typed(repo, |tx| {
        Box::pin(async move {
            let tracks = sqlx::query_scalar("SELECT COUNT(*) FROM tracks")
                .fetch_one(&mut **tx)
                .await?;
            let folders = sqlx::query_scalar("SELECT COUNT(*) FROM area_folders")
                .fetch_one(&mut **tx)
                .await?;
            let planner_cards =
                sqlx::query_scalar("SELECT COUNT(*) FROM cards WHERE role='planner'")
                    .fetch_one(&mut **tx)
                    .await?;
            let report_cards =
                sqlx::query_scalar("SELECT COUNT(*) FROM cards WHERE role='reportcard'")
                    .fetch_one(&mut **tx)
                    .await?;
            let overlays = sqlx::query_scalar("SELECT COUNT(*) FROM overlays")
                .fetch_one(&mut **tx)
                .await?;
            let tasks = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
                .fetch_one(&mut **tx)
                .await?;
            Ok((
                tracks,
                folders,
                planner_cards,
                report_cards,
                overlays,
                tasks,
            ))
        })
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn fork_preserves_block_truth_and_rewrites_only_internal_references() {
    let boot = boot().await;
    let (status, source_report) = request_json(
        &boot.app,
        "GET",
        format!("/api/tracks/{}/report", boot.source_track_id),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let source_truth = block_index(&source_report);
    assert_eq!(source_truth.len(), 8);
    let source_markdown = source_report["blocks"][0]["payload"]["markdown"]
        .as_str()
        .unwrap();
    assert!(
        source_markdown.len() >= 9 * 1024,
        "source markdown fixture shrank below 9 KiB: {} bytes",
        source_markdown.len()
    );
    let internal_block_id = source_truth[2].0.clone();

    let (status, target_track) = request_json(
        &boot.app,
        "POST",
        "/api/tracks".into(),
        &boot.cookie,
        Some(json!({
            "area_id": boot.area_id,
            "title": "fork target",
            "sort": null,
            "cwd": target_cwd(""),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_track_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {target_track}");
    let target_track_id = target_track["id"].as_str().unwrap().to_string();

    let (status, target_report) = request_json(
        &boot.app,
        "GET",
        format!("/api/tracks/{target_track_id}/report"),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(block_index(&target_report), source_truth);
    assert_eq!(target_report["summary"], "fork source summary");
    assert_eq!(target_report["docRev"], 0);

    let expected_markdown = format!(
        concat!(
            "# Fixture\n\n{long}\n\n",
            "[inline](neige://wave/{0}#{1})\n",
            "[second-inline](neige://wave/{0}#{1})\n",
            "[reference][same]\n",
            "<neige://wave/{0}#{1}>\n",
            "[dangling](neige://wave/{2}#b_dead)\n",
            "`[code](neige://wave/{2}#b_dead)`\n",
            "```markdown\n[fenced](neige://wave/{2}#b_dead)\n```\n",
            "[external](neige://wave/external-track#b_4444)\n",
            "\n[same]: neige://wave/{0}#{1}\n",
        ),
        target_track_id,
        internal_block_id,
        boot.source_track_id,
        long = long_fixture_text(),
    );
    let expected_blocks = vec![
        json!({
            "id": source_truth[0].0,
            "kind": "prose",
            "rev": source_truth[0].1,
            "payload": {"markdown": expected_markdown}
        }),
        json!({
            "id": source_truth[1].0,
            "kind": "chart.candles",
            "rev": source_truth[1].1,
            "payload": {
                "symbol": "985.TEST", "period": "day",
                "candles": [[1719800000000_i64, 10, 12, 9, 11, 100], [1719886400000_i64, 11, 13, 10, 12, 120]],
                "overlays": ["ma20", "ma60"], "caption": "fixture chart"
            }
        }),
        json!({
            "id": source_truth[2].0,
            "kind": "prose",
            "rev": source_truth[2].1,
            "payload": {"markdown": "# Anchor\n\nanchor payload\n"}
        }),
        json!({
            "id": source_truth[3].0,
            "kind": "table",
            "rev": source_truth[3].1,
            "payload": {
                "columns": [
                    {"key": "name", "label": "Name", "align": "left"},
                    {"key": "value", "label": "Value", "align": "right"}
                ],
                "rows": [{"name": "alpha", "value": 1}, {"name": "beta", "value": null}],
                "caption": "fixture table", "highlight": "alpha"
            }
        }),
        json!({
            "id": source_truth[4].0,
            "kind": "app",
            "rev": source_truth[4].1,
            "payload": {"src": "/apps/fork-fixture?mode=deep", "title": "Fixture app", "height": 640}
        }),
        json!({
            "id": source_truth[5].0,
            "kind": "task",
            "rev": source_truth[5].1,
            "payload": {
                "key": "build", "kind": "codex",
                "goal": format!("Goal [internal](neige://wave/{target_track_id}#{internal_block_id})"),
                "acceptance": format!("Accept <neige://wave/{target_track_id}#{internal_block_id}>") ,
                "refs": [internal_block_ref(&target_track_id, &internal_block_id)],
                "ready": false, "declared_by": "spec"
            }
        }),
        json!({
            "id": source_truth[6].0,
            "kind": "task",
            "rev": source_truth[6].1,
            "payload": {
                "key": "build", "kind": "terminal",
                "goal": "Second declaration with the same key",
                "acceptance": "Second declaration remains exact",
                "refs": [], "ready": false, "declared_by": "spec"
            }
        }),
        json!({
            "id": source_truth[7].0,
            "kind": "task",
            "rev": source_truth[7].1,
            "payload": {
                // #1111 — the copy is planner-owned on BOTH privilege fields.
                "key": "rejected", "tombstone": {"reason": "not now"},
                "declared_by": "spec", "tombstoned_by": "spec"
            }
        }),
    ];
    let actual_blocks = target_report["blocks"].as_array().unwrap();
    let target_markdown = actual_blocks[0]["payload"]["markdown"].as_str().unwrap();
    assert!(
        target_markdown.len() >= 9 * 1024,
        "target markdown fixture shrank below 9 KiB: {} bytes",
        target_markdown.len()
    );
    assert_eq!(actual_blocks.len(), expected_blocks.len());
    for (index, (actual, expected)) in actual_blocks.iter().zip(&expected_blocks).enumerate() {
        assert_eq!(actual, expected, "forked block {index} drifted");
    }
    let diagnostics = target_report["taskDiagnostics"].as_array().unwrap();
    assert!(
        diagnostics
            .iter()
            .flat_map(|verdict| verdict["diagnostics"].as_array().unwrap())
            .all(|diagnostic| diagnostic["code"] != "reference_missing"),
        "fork projection observed a missing copied-block reference: {diagnostics:?}"
    );

    let report_card = boot
        .repo
        .cards_by_track(&target_track_id)
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.kind == "track-report")
        .unwrap();
    let mut payload = report_card.payload;
    payload.as_object_mut().unwrap().remove("blocks").unwrap();
    let card_id = report_card.id.to_string();
    let payload = serde_json::to_string(&payload).unwrap();
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            sqlx::query("UPDATE cards SET payload=?1 WHERE id=?2")
                .bind(payload)
                .bind(card_id)
                .execute(&mut **tx)
                .await?;
            Ok(())
        })
    })
    .await
    .unwrap();

    let (status, crdt_report) = request_json(
        &boot.app,
        "GET",
        format!("/api/tracks/{target_track_id}/report"),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(crdt_report["blocks"], Value::Array(expected_blocks));
    assert_eq!(crdt_report["summary"], "fork source summary");

    // Keep fixture fields load-bearing: the source card stayed untouched and
    // the app state remained alive through the post-create operation wait.
    let (status, source_after) = request_json(
        &boot.app,
        "GET",
        format!("/api/tracks/{}/report", boot.source_track_id),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        source_after, source_report,
        "fork mutated its source report"
    );
    let _ = &boot.state;
}

#[tokio::test]
async fn legacy_source_without_crdt_or_block_cache_forks_once_without_remint_or_source_write() {
    let boot = boot().await;
    // Frozen schema-v1 wire payload. `docRev` and `blocks` did not exist in
    // that schema; constructing this through today's TrackReportPayload would
    // manufacture an unreachable v3+NULL combination.
    const LEGACY_V1_PAYLOAD_JSON: &str =
        r##"{"schemaVersion":1,"summary":"legacy summary","body":"# Legacy\n\nlegacy block\n"}"##;
    let report_id = boot.source_report_id.clone();
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            sqlx::query("UPDATE cards SET payload=json(?1),body_crdt=NULL WHERE id=?2")
                .bind(LEGACY_V1_PAYLOAD_JSON)
                .bind(report_id)
                .execute(&mut **tx)
                .await?;
            Ok(())
        })
    })
    .await
    .unwrap();

    let raw_before: (String, Option<Vec<u8>>) =
        calm_server::db::write_in_tx_typed(boot.repo.as_ref(), {
            let report_id = boot.source_report_id.clone();
            move |tx| {
                Box::pin(async move {
                    Ok(
                        sqlx::query_as("SELECT json(payload),body_crdt FROM cards WHERE id=?1")
                            .bind(report_id)
                            .fetch_one(&mut **tx)
                            .await?,
                    )
                })
            }
        })
        .await
        .unwrap();
    assert!(raw_before.1.is_none());
    assert!(
        serde_json::from_str::<Value>(&raw_before.0).unwrap()["blocks"].is_null(),
        "legacy source must exercise absent payload.blocks"
    );
    let (status, source_report) = request_json(
        &boot.app,
        "GET",
        format!("/api/tracks/{}/report", boot.source_track_id),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let source_truth = block_index(&source_report);
    assert_eq!(source_truth.len(), 1);
    assert_eq!(source_report["schemaVersion"], 1);

    let (status, target_track) = request_json(
        &boot.app,
        "POST",
        "/api/tracks".into(),
        &boot.cookie,
        Some(json!({
            "area_id": boot.area_id,
            "title": "legacy fork target",
            "sort": null,
            "cwd": target_cwd("-legacy"),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_track_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {target_track}");
    let target_id = target_track["id"].as_str().unwrap();
    let (status, target_report) = request_json(
        &boot.app,
        "GET",
        format!("/api/tracks/{target_id}/report"),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        block_index(&target_report),
        source_truth,
        "legacy ids reminted"
    );
    assert_eq!(target_report["summary"], "legacy summary");
    assert_eq!(
        target_report["blocks"],
        json!([{"id": source_truth[0].0, "kind": "prose", "rev": source_truth[0].1,
                "payload": {"markdown": "# Legacy\n\nlegacy block\n"}}])
    );
    let raw_after: (String, Option<Vec<u8>>) =
        calm_server::db::write_in_tx_typed(boot.repo.as_ref(), {
            let report_id = boot.source_report_id.clone();
            move |tx| {
                Box::pin(async move {
                    Ok(
                        sqlx::query_as("SELECT json(payload),body_crdt FROM cards WHERE id=?1")
                            .bind(report_id)
                            .fetch_one(&mut **tx)
                            .await?,
                    )
                })
            }
        })
        .await
        .unwrap();
    assert_eq!(
        raw_after, raw_before,
        "legacy fork wrote back to its source"
    );
}

#[tokio::test]
async fn canonical_fresh_null_crdt_source_forks_through_the_rest_path() {
    let boot = boot().await;
    let (status, source_track) = request_json(
        &boot.app,
        "POST",
        "/api/tracks".into(),
        &boot.cookie,
        Some(json!({
            "area_id": boot.area_id,
            "title": "canonical fresh source",
            "sort": null,
            "cwd": target_cwd("-canonical-source"),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {source_track}");
    let source_id = source_track["id"].as_str().unwrap();
    let source_card = boot
        .repo
        .cards_by_track(source_id)
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.kind == "track-report")
        .unwrap();
    let raw_source = boot
        .repo
        .card_get_with_body_crdt(source_card.id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        raw_source.0.payload,
        serde_json::to_value(TrackReportPayload::initial()).unwrap()
    );
    assert!(raw_source.1.is_none(), "fresh report unexpectedly had CRDT");

    let (status, source_report) = request_json(
        &boot.app,
        "GET",
        format!("/api/tracks/{source_id}/report"),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, target_track) = request_json(
        &boot.app,
        "POST",
        "/api/tracks".into(),
        &boot.cookie,
        Some(json!({
            "area_id": boot.area_id,
            "title": "canonical fresh target",
            "sort": null,
            "cwd": target_cwd("-canonical-target"),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": source_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {target_track}");
    let target_id = target_track["id"].as_str().unwrap();
    let (status, target_report) = request_json(
        &boot.app,
        "GET",
        format!("/api/tracks/{target_id}/report"),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(target_report["schemaVersion"], 3);
    assert_eq!(target_report["docRev"], 0);
    assert_eq!(target_report["summary"], source_report["summary"]);
    assert_eq!(target_report["body"], source_report["body"]);
    assert_eq!(target_report["blocks"], source_report["blocks"]);
}

#[tokio::test]
async fn empty_block_snapshot_written_by_rest_forks_payload_and_crdt_exactly() {
    let boot = boot().await;
    let (status, source_track) = request_json(
        &boot.app,
        "POST",
        "/api/tracks".into(),
        &boot.cookie,
        Some(json!({
            "area_id": boot.area_id,
            "title": "empty source",
            "sort": null,
            "cwd": target_cwd("-empty-source"),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {source_track}");
    let source_id = source_track["id"].as_str().unwrap();
    let (status, fresh_report) = request_json(
        &boot.app,
        "GET",
        format!("/api/tracks/{source_id}/report"),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // #1185: the birth report is a five-block skeleton, so emptying it means
    // deleting every block, re-reading between deletes for the fresh `rev`.
    let mut report = fresh_report;
    loop {
        let blocks = report["blocks"].as_array().expect("blocks array").clone();
        let Some(seed) = blocks.first() else { break };
        let (status, empty_write) = request_json(
            &boot.app,
            "DELETE",
            format!(
                "/api/tracks/{source_id}/report/blocks/{}",
                seed["id"].as_str().unwrap()
            ),
            &boot.cookie,
            Some(json!({"ifBlockRev": seed["rev"]})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "empty report write: {empty_write}");
        let (status, next) = request_json(
            &boot.app,
            "GET",
            format!("/api/tracks/{source_id}/report"),
            &boot.cookie,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        report = next;
    }
    let (status, source_report) = request_json(
        &boot.app,
        "GET",
        format!("/api/tracks/{source_id}/report"),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(source_report["blocks"], json!([]));

    let (status, target_track) = request_json(
        &boot.app,
        "POST",
        "/api/tracks".into(),
        &boot.cookie,
        Some(json!({
            "area_id": boot.area_id,
            "title": "empty target",
            "sort": null,
            "cwd": target_cwd("-empty-target"),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": source_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {target_track}");
    let target_id = target_track["id"].as_str().unwrap();
    let target_card = boot
        .repo
        .cards_by_track(target_id)
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.kind == "track-report")
        .unwrap();
    assert_eq!(
        target_card.payload,
        json!({
            "schemaVersion": 3,
            "docRev": 0,
            "summary": "",
            "body": "",
            "blocks": [],
        })
    );
    let (_, target_crdt) = boot
        .repo
        .card_get_with_body_crdt(target_card.id.as_str())
        .await
        .unwrap()
        .unwrap();
    let target_doc = calm_server::track_report_doc::ReportDoc::from_bytes(
        target_crdt.as_deref().expect("fork target CRDT missing"),
    )
    .unwrap();
    assert_eq!(target_doc.doc_rev().unwrap(), 0);
    assert_eq!(
        target_doc.project().unwrap(),
        (String::new(), String::new())
    );
    assert_eq!(
        target_doc.blocks_snapshot().unwrap(),
        Vec::<calm_types::track_report::ReportBlock>::new()
    );
}

#[tokio::test]
async fn invalid_fork_payload_rest_path_rolls_back_every_created_row() {
    let boot = boot().await;
    let invalid_body = render_fence("app", &json!({"src": "https://example.invalid/app"}));
    let invalid_payload = TrackReportPayload::new("invalid fork source", invalid_body);
    let report_id = boot.source_report_id.clone();
    let serialized = serde_json::to_string(&invalid_payload).unwrap();
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            sqlx::query("UPDATE cards SET payload=json(?1),body_crdt=NULL WHERE id=?2")
                .bind(serialized)
                .bind(report_id)
                .execute(&mut **tx)
                .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    let before = fork_row_counts(boot.repo.as_ref()).await;
    let (status, body) = request_json(
        &boot.app,
        "POST",
        "/api/tracks".into(),
        &boot.cookie,
        Some(json!({
            "area_id": boot.area_id,
            "title": "invalid payload target",
            "sort": null,
            "cwd": target_cwd("-invalid-payload"),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_track_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body = {body}");
    assert!(body.to_string().contains("src: required same-origin path"));
    let after = fork_row_counts(boot.repo.as_ref()).await;
    assert_eq!(after.0, before.0, "failed fork left a tracks row");
    assert_eq!(after.1, before.1, "failed fork left a area_folders row");
    assert_eq!(after.2, before.2, "failed fork left a planner card");
    assert_eq!(after.3, before.3, "failed fork left a report card");
    assert_eq!(after.4, before.4, "failed fork left an overlay");
    assert_eq!(after.5, before.5, "failed fork left a tasks row");
}

/// Issue #1111 — the companion half of the tombstone normalization: fork
/// rewrites `tombstoned_by` **only** on tombstone blocks. A residual
/// `tombstoned_by` on a *non*-tombstone task is deliberately left alone so the
/// fork's own `validate_payload` breaks the whole track creation fail-closed,
/// rather than silently repairing a corrupt source into a shape it never
/// validly had.
///
/// **This shape is unreachable in production — this is not a realistic
/// regression.** Every write surface that can produce a stored report runs
/// `validate_payload` first: the whole-document path via
/// `track_report.rs:392,409 → track_report_guard.rs:38`, and the block-level
/// upsert path via `calm-types/src/report_blocks/mod.rs:214-216`. Nor can
/// `normalize_report_op` emit it. The fixture below therefore forges the shape
/// with raw SQL (`UPDATE cards ...`), bypassing every production writer.
///
/// What it pins is the **fail-closed safety net against legacy rows or a
/// database corrupted from outside the server**: should such a payload ever
/// reach fork, the whole creation must abort with zero residue rather than be
/// quietly normalized. Do not read this test as a production regression.
#[tokio::test]
async fn fork_fails_closed_on_residual_tombstoned_by_on_a_live_task() {
    let boot = boot().await;
    let residual_body = render_fence(
        "task",
        &json!({
            "key": "build",
            "kind": "codex",
            "goal": "Live task carrying a residual tombstone author",
            "acceptance": "Fork must refuse this shape outright",
            "refs": [],
            "ready": true,
            "declared_by": "user",
            "tombstoned_by": "user"
        }),
    );
    let residual_payload = TrackReportPayload::new("residual tombstoned_by source", residual_body);
    let report_id = boot.source_report_id.clone();
    let serialized = serde_json::to_string(&residual_payload).unwrap();
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            sqlx::query("UPDATE cards SET payload=json(?1),body_crdt=NULL WHERE id=?2")
                .bind(serialized)
                .bind(report_id)
                .execute(&mut **tx)
                .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    let before = fork_row_counts(boot.repo.as_ref()).await;
    let (status, body) = request_json(
        &boot.app,
        "POST",
        "/api/tracks".into(),
        &boot.cookie,
        Some(json!({
            "area_id": boot.area_id,
            "title": "residual tombstoned_by target",
            "sort": null,
            "cwd": target_cwd("-residual-tombstoned-by"),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_track_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body = {body}");
    assert!(
        body.to_string()
            .contains("must be absent from a non-tombstone task"),
        "body = {body}"
    );
    let after = fork_row_counts(boot.repo.as_ref()).await;
    assert_eq!(after.0, before.0, "failed fork left a tracks row");
    assert_eq!(after.1, before.1, "failed fork left a area_folders row");
    assert_eq!(after.2, before.2, "failed fork left a planner card");
    assert_eq!(after.3, before.3, "failed fork left a report card");
    assert_eq!(after.4, before.4, "failed fork left an overlay");
    assert_eq!(after.5, before.5, "failed fork left a tasks row");
}

#[tokio::test]
async fn unsafe_markdown_destinations_fail_fork_with_block_and_source() {
    let boot = boot().await;
    let (status, report) = request_json(
        &boot.app,
        "GET",
        format!("/api/tracks/{}/report", boot.source_track_id),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let target_block_id = report["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|block| block["kind"] == "prose")
        .nth(1)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let entity_destination = format!(
        "neige://wave/{}#{target_block_id}",
        entity_encode_first(&boot.source_track_id)
    );
    let escaped_destination = format!("neige\\://wave/{}#{target_block_id}", boot.source_track_id);
    let html_destination = format!("neige://wave/{}#{target_block_id}", boot.source_track_id);
    let cases = [
        format!("[entity]({entity_destination})"),
        format!("[escaped]({escaped_destination})"),
        format!(r#"[<span title="[">label</span>]({html_destination})"#),
    ];

    for (index, (markdown, destination)) in cases
        .into_iter()
        .zip([entity_destination, escaped_destination, html_destination])
        .enumerate()
    {
        let (status, report) = request_json(
            &boot.app,
            "GET",
            format!("/api/tracks/{}/report", boot.source_track_id),
            &boot.cookie,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let prose = report["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|block| block["kind"] == "prose")
            .unwrap();
        let prose_id = prose["id"].as_str().unwrap();
        let (status, response) = request_json(
            &boot.app,
            "PATCH",
            format!(
                "/api/tracks/{}/report/blocks/{prose_id}",
                boot.source_track_id
            ),
            &boot.cookie,
            Some(json!({
                "kind": "prose",
                "markdown": markdown,
                "ifBlockRev": prose["rev"]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "fixture update: {response}");

        let (status, body) = request_json(
            &boot.app,
            "POST",
            "/api/tracks".into(),
            &boot.cookie,
            Some(json!({
                "area_id": boot.area_id,
                "title": format!("unsafe fork {index}"),
                "sort": null,
                "cwd": target_cwd(&format!("-unsafe-{index}")),
                "attach_folder": true,
                "theme": routes::theme::RequestTheme::default_dark(),
                "fork_report_from": boot.source_track_id,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body = {body}");
        let error = body["error"].as_str().unwrap();
        assert!(error.contains(prose_id), "{error}");
        assert!(error.contains(&destination), "{error}");
        assert!(error.contains("plain form"), "{error}");
    }
}

/// Issue #1111 — a forked task tombstone must not carry the template's
/// `tombstoned_by: "user"` privilege into every track forked from it.
///
/// The fixture report holds a tombstone declared AND tombstoned by the user.
/// After a fork, the planner author owns the copy: the guard's `user_owned`
/// disjunction (`declared_by == "user" || tombstoned_by == "user"`) plus the
/// immutability of `tombstoned_by` would otherwise freeze that block forever.
#[tokio::test]
async fn forked_user_tombstone_is_normalized_to_planner_and_stays_planner_editable() {
    let boot = boot().await;
    let (status, target_track) = request_json(
        &boot.app,
        "POST",
        "/api/tracks".into(),
        &boot.cookie,
        Some(json!({
            "area_id": boot.area_id,
            "title": "tombstone fork target",
            "sort": null,
            "cwd": target_cwd("-tombstone"),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_track_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {target_track}");
    let target_track_id = target_track["id"].as_str().unwrap().to_string();

    let (status, target_report) = request_json(
        &boot.app,
        "GET",
        format!("/api/tracks/{target_track_id}/report"),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let tombstone = target_report["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["kind"] == "task" && block["payload"]["key"] == "rejected")
        .expect("forked report keeps the tombstone block")
        .clone();
    assert!(
        !tombstone["payload"]["tombstone"].is_null(),
        "fixture block must be a tombstone: {tombstone}"
    );

    // Drive the real planner write path (MCP `calm.report.blocks.*` →
    // `CardDecisionSink::commit_report_op` → `guard_task_declarations`).
    let (ctx, registry, identity) = planner_tool_channel(&boot, &target_track_id).await;
    let rewritten = json!({
        "key": "rejected",
        "tombstone": { "reason": "planner re-scoped this task" },
        "declared_by": tombstone["payload"]["declared_by"].clone(),
        "tombstoned_by": tombstone["payload"]["tombstoned_by"].clone()
    });
    let upsert = call_planner_tool(
        &ctx,
        &registry,
        TOOL_REPORT_BLOCKS_UPSERT,
        identity.clone(),
        json!({
            "id": tombstone["id"],
            "kind": "task",
            "payload": rewritten,
            "if_rev": tombstone["rev"]
        }),
    )
    .await
    .expect("planner author must be able to rewrite a forked tombstone");

    let delete = call_planner_tool(
        &ctx,
        &registry,
        TOOL_REPORT_BLOCKS_DELETE,
        identity,
        json!({ "id": tombstone["id"], "if_rev": upsert["rev"] }),
    )
    .await
    .expect("planner author must be able to delete a forked tombstone");
    assert!(delete.get("docRev").is_some(), "delete returns docRev");

    let (status, after) = request_json(
        &boot.app,
        "GET",
        format!("/api/tracks/{target_track_id}/report"),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !after["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|block| block["id"] == tombstone["id"]),
        "the planner-owned tombstone is gone after the planner delete: {after}"
    );
    assert_eq!(
        tombstone["payload"]["tombstoned_by"], "spec",
        "fork must re-attribute the copied tombstone to the planner author: {tombstone}"
    );
}

const FORKED_PLANNER_SESSION_ID: &str = "forked-planner-session";

fn planner_session(id: &str, track_id: TrackId, card_id: CardId) -> WorkerSession {
    WorkerSession {
        id: WorkerSessionId::from(id),
        track_id,
        provider: WorkerProviderKind::Codex,
        mode: SessionMode::Resumable,
        contract: WorkerContract::Planner,
        parent_session_id: None,
        requester_session_id: None,
        state: WorkerSessionState::Starting,
        mcp_token_hash: None,
        thread_id: None,
        agent_session_id: None,
        active_turn_id: None,
        terminal_run_id: None,
        card_id: Some(card_id),
        handle_state_json: None,
        liveness: LivenessTag::Unknown,
        liveness_probed_at_ms: None,
        exit_code: None,
        exit_interpretation: None,
        spawn_op_id: None,
        last_activity_ms: None,
        last_thread_status: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    }
}

/// An MCP tool channel bound to the forked track's own planner card — the
/// production identity a planner agent writes its track report through.
async fn planner_tool_channel(
    boot: &Boot,
    track_id: &str,
) -> (Arc<AppContext>, Arc<ToolRegistry>, ToolCallIdentity) {
    let planner_card = boot
        .repo
        .cards_by_track(track_id)
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.kind == "codex")
        .expect("forked track has a planner card");
    let session = planner_session(
        FORKED_PLANNER_SESSION_ID,
        TrackId::from(track_id.to_string()),
        planner_card.id.clone(),
    );
    let root_session_id = session.id.clone();
    let root_track_id = TrackId::from(track_id.to_string());
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            session_insert_tx(tx, session)
                .await
                .map_err(CalmError::from)?;
            session_mark_track_root_tx(tx, &root_track_id, &root_session_id)
                .await
                .map_err(CalmError::from)?;
            Ok(())
        })
    })
    .await
    .expect("seed the forked track's root planner session");

    let route_repo: Arc<dyn calm_server::db::RouteRepo> = boot.repo.clone();
    let ctx = Arc::new(AppContext {
        repo: route_repo,
        track_vcs: None,
        events: boot.state.events.clone(),
        write: boot.state.write().clone(),
        daemon_token_hash: None,
        gate_logs_dir: std::env::temp_dir().join("neige-test-gate-logs"),
        plugin_host: Arc::new(tokio::sync::OnceCell::new()),
        operation_runtime: Arc::new(tokio::sync::OnceCell::new()),
    });
    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);
    let identity = ToolCallIdentity {
        card_id: planner_card.id.as_str().to_string(),
        role: CardRole::Planner,
        provider: AgentProvider::Codex,
        session_id: FORKED_PLANNER_SESSION_ID.to_string(),
        track_id: Some(track_id.to_string()),
        area_id: boot.area_id.clone(),
        thread_id: "forked-planner-thread".to_string(),
    };
    (ctx, Arc::new(registry), identity)
}

async fn call_planner_tool(
    ctx: &Arc<AppContext>,
    registry: &Arc<ToolRegistry>,
    name: &str,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, calm_server::plugin_host::mcp::RpcError> {
    let handler = registry
        .lookup(name)
        .unwrap_or_else(|| panic!("tool not registered: {name}"));
    handler(ctx.clone(), identity, args).await
}

/// The task payload the source track's user declares. `declared_by: "user"` is
/// the only shape the REST user path may create (Rule 1); fork rewrites it to
/// `"planner"`, which is exactly the shape `declare_and_wait` is meant to hold.
fn released_fixture_payload(key: &str) -> Value {
    json!({
        "key": key,
        "kind": "codex",
        "goal": "Template task the source track's user allowed",
        "acceptance": "A track forked from this template must ask its own user again",
        "refs": [],
        // Kept gate-clean on purpose: `declare_and_wait` must be the ONLY
        // diagnostic this fixture can produce, so `schedulable` below is a
        // signal about the release and not about a missing gate.
        "no_gate_reason": "fixture task carries no gate",
        "ready": true,
        "declared_by": "user"
    })
}

/// Seed a live task that the SOURCE track's user declared and then released,
/// through the production REST user path: `POST .../report/blocks` followed by
/// the same `PATCH` the "Allow this task" button issues
/// (`TrackReportPage.tsx:95-99` spreads the payload and sets
/// `released_by_user: true`). Returns the seeded block id.
async fn seed_user_released_task(boot: &Boot, key: &str) -> String {
    let (status, report) = request_json(
        &boot.app,
        "GET",
        format!("/api/tracks/{}/report", boot.source_track_id),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "read source report: {report}");
    let (status, created) = request_json(
        &boot.app,
        "POST",
        format!("/api/tracks/{}/report/blocks", boot.source_track_id),
        &boot.cookie,
        Some(json!({
            "kind": "task",
            "payload": released_fixture_payload(key),
            "ifDocRev": report["docRev"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seed task block: {created}");
    let block_id = created["id"].as_str().unwrap().to_string();
    let mut released = released_fixture_payload(key);
    released["released_by_user"] = json!(true);
    let (status, body) = request_json(
        &boot.app,
        "PATCH",
        format!(
            "/api/tracks/{}/report/blocks/{block_id}",
            boot.source_track_id
        ),
        &boot.cookie,
        Some(json!({
            "kind": "task",
            "payload": released,
            "ifBlockRev": created["rev"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seed user release: {body}");
    block_id
}

fn task_block_by_key<'a>(report: &'a Value, key: &str) -> &'a Value {
    report["blocks"]
        .as_array()
        .expect("blocks array")
        .iter()
        .find(|block| block["kind"] == "task" && block["payload"]["key"] == key)
        .unwrap_or_else(|| panic!("forked report keeps task `{key}`: {report}"))
}

fn verdict_by_key<'a>(report: &'a Value, key: &str) -> &'a Value {
    report["taskDiagnostics"]
        .as_array()
        .expect("taskDiagnostics array")
        .iter()
        .find(|verdict| verdict["key"] == key)
        .unwrap_or_else(|| panic!("verdict for `{key}`: {report}"))
}

/// Issue #1115 — a template's `released_by_user: true` must not carry the
/// SOURCE user's consent into a track forked from it.
///
/// Fork rewrites `declared_by` to `"spec"` (§7.2), which is precisely the shape
/// `declare_and_wait` exists to hold back
/// (`task_projection.rs:709-719`: `effective_wait && declared_by == "spec" &&
/// !released_by_user && !tombstone`). Copying the release flag verbatim exempts
/// the copy from a decision the new track's user never made — and
/// `report-blocks/task.tsx:185` then hides the "Allow this task" button, so she
/// cannot even see the exemption.
///
/// Whole flow through production REST: seed + release in the source, fork as the
/// browser does (no `X-Calm-Actor`), tighten the new track to `declare-and-wait`,
/// then let the new user mark the copy ready — the only remaining gate must be
/// her own release.
#[tokio::test]
async fn forked_task_does_not_inherit_the_source_users_release() {
    let boot = boot().await;
    seed_user_released_task(&boot, "allowed-upstream").await;

    let (status, target_track) = request_json(
        &boot.app,
        "POST",
        "/api/tracks".into(),
        &boot.cookie,
        Some(json!({
            "area_id": boot.area_id,
            "title": "release normalization target",
            "sort": null,
            "cwd": target_cwd("-released"),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_track_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {target_track}");
    let target_track_id = target_track["id"].as_str().unwrap().to_string();

    let (status, body) = request_json(
        &boot.app,
        "PATCH",
        format!("/api/tracks/{target_track_id}"),
        &boot.cookie,
        Some(json!({ "automation_policy": "declare-and-wait" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "tighten policy: {body}");

    let (status, report) = request_json(
        &boot.app,
        "GET",
        format!("/api/tracks/{target_track_id}/report"),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let forked = task_block_by_key(&report, "allowed-upstream").clone();
    assert_eq!(
        forked["payload"]["declared_by"], "spec",
        "fork re-attributes the copy to the planner author: {forked}"
    );
    assert_ne!(
        forked["payload"]["released_by_user"],
        json!(true),
        "fork must not carry the source user's release into the new track: {forked}"
    );

    // The new track's user marks the copy ready — the ONLY thing that may still
    // hold it back is her own release, so `schedulable` is a live signal here
    // rather than a by-product of the forced `ready: false`.
    let mut ready = forked["payload"].clone();
    ready["ready"] = json!(true);
    let (status, body) = request_json(
        &boot.app,
        "PATCH",
        format!(
            "/api/tracks/{target_track_id}/report/blocks/{}",
            forked["id"].as_str().unwrap()
        ),
        &boot.cookie,
        Some(json!({
            "kind": "task",
            "payload": ready,
            "ifBlockRev": forked["rev"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "mark the forked copy ready: {body}");

    let (status, report) = request_json(
        &boot.app,
        "GET",
        format!("/api/tracks/{target_track_id}/report"),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let verdict = verdict_by_key(&report, "allowed-upstream");
    assert!(
        verdict["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == "declare_and_wait"),
        "the forked copy must wait for the NEW track's user: {verdict}"
    );
    assert_eq!(
        verdict["schedulable"],
        json!(false),
        "an unreleased declare-and-wait task is not schedulable: {verdict}"
    );
}

/// Issue #1115 — the tombstone half of the release normalization.
///
/// Fork strips `released_by_user` **only** from live task blocks. It must not
/// insert or clear anything on a tombstone, because the tombstone schema
/// (`calm-types/src/report_blocks/kinds.rs`) is the closed shape
/// `{key, tombstone, declared_by, tombstoned_by}` and rejects every other
/// accepted task field with `must be absent from a tombstone task`. A residual
/// `released_by_user` on a tombstone therefore breaks the whole fork
/// fail-closed, exactly as a residual `tombstoned_by` on a live task does
/// (#1111) — the same deliberate choice: a corrupt source aborts track creation
/// instead of being silently repaired into a shape it never validly had.
///
/// **This shape is unreachable in production — not a realistic regression.**
/// Every stored-report writer runs `validate_payload` first (whole-document via
/// `track_report_guard.rs`, block-level via `report_blocks/mod.rs`), and the
/// tombstone-rewrite in `normalize_report_op` emits the closed shape only. The
/// fixture forges it with a raw `UPDATE cards`. What it pins is the
/// fail-closed net for legacy rows or a DB corrupted from outside the server,
/// plus the fact that the normalization stays in the live-task arm.
#[tokio::test]
async fn fork_fails_closed_on_a_tombstone_carrying_released_by_user() {
    let boot = boot().await;
    let residual_body = render_fence(
        "task",
        &json!({
            "key": "rejected",
            "tombstone": { "reason": "not now" },
            "declared_by": "user",
            "tombstoned_by": "user",
            "released_by_user": true
        }),
    );
    let residual_payload = TrackReportPayload::new("released tombstone source", residual_body);
    let report_id = boot.source_report_id.clone();
    let serialized = serde_json::to_string(&residual_payload).unwrap();
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            sqlx::query("UPDATE cards SET payload=json(?1),body_crdt=NULL WHERE id=?2")
                .bind(serialized)
                .bind(report_id)
                .execute(&mut **tx)
                .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    let before = fork_row_counts(boot.repo.as_ref()).await;
    let (status, body) = request_json(
        &boot.app,
        "POST",
        "/api/tracks".into(),
        &boot.cookie,
        Some(json!({
            "area_id": boot.area_id,
            "title": "released tombstone target",
            "sort": null,
            "cwd": target_cwd("-released-tombstone"),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_track_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body = {body}");
    assert!(
        body.to_string()
            .contains("released_by_user: must be absent from a tombstone task"),
        "body = {body}"
    );
    let after = fork_row_counts(boot.repo.as_ref()).await;
    assert_eq!(after.0, before.0, "failed fork left a tracks row");
    assert_eq!(after.1, before.1, "failed fork left a area_folders row");
    assert_eq!(after.2, before.2, "failed fork left a planner card");
    assert_eq!(after.3, before.3, "failed fork left a report card");
    assert_eq!(after.4, before.4, "failed fork left an overlay");
    assert_eq!(after.5, before.5, "failed fork left a tasks row");
}

/// INV-1110-002: a forked track's `neige state` / `calm.track.state` bit is
/// true, and the planner developer instructions (first-turn prompt) tell the
/// agent to `calm.report.read` when that bit is set. This is not a live
/// Codex turn; the fixture's planner harness uses a nonexistent binary.
#[tokio::test]
async fn inv_1110_002_forked_track_requires_report_startup_read() {
    let boot = boot().await;
    let (status, target_track) = request_json(
        &boot.app,
        "POST",
        "/api/tracks".into(),
        &boot.cookie,
        Some(json!({
            "area_id": boot.area_id,
            "title": "startup-read fork target",
            "sort": null,
            "cwd": target_cwd("-startup-read"),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_track_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {target_track}");
    let target_track_id = target_track["id"].as_str().unwrap().to_string();

    let report_card = boot
        .repo
        .cards_by_track(&target_track_id)
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.kind == "track-report")
        .expect("forked track has a report card");
    let payload: TrackReportPayload =
        serde_json::from_value(report_card.payload).expect("forked report payload");
    assert!(
        payload.report_startup_read_required(),
        "forked report content must differ from TrackReportPayload::initial()"
    );

    let (ctx, registry, identity) = planner_tool_channel(&boot, &target_track_id).await;
    let state = call_planner_tool(&ctx, &registry, TOOL_TRACK_STATE, identity, json!({}))
        .await
        .expect("forked planner can read track state");
    assert_eq!(
        state
            .get("report_startup_read_required")
            .and_then(Value::as_bool),
        Some(true),
        "forked track state must require a startup report read: {state}"
    );

    let prompt =
        render_planner_developer_instructions_for_test(target_track_id.as_str(), None, None);
    // #1185 §1.5 A — the read is unconditional now. The bit above still
    // matters, and matters more: its meaning narrowed from "must you read" to
    // "does this document already hold content beyond the default skeleton",
    // which is exactly what a forked report is.
    assert!(
        prompt.contains(
            "Before you write anything to the report in a session, call `calm.report.read` once"
        ),
        "planner first-turn prompt must mandate an unconditional first read"
    );
    assert!(prompt.contains("authoritative pre-set plan"));
    assert!(prompt.contains("Do not mint duplicate tasks"));
}

#[tokio::test]
async fn initial_track_does_not_require_report_startup_read() {
    let boot = boot().await;
    let (status, source_track) = request_json(
        &boot.app,
        "POST",
        "/api/tracks".into(),
        &boot.cookie,
        Some(json!({
            "area_id": boot.area_id,
            "title": "canonical initial source",
            "sort": null,
            "cwd": target_cwd("-initial-state"),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {source_track}");
    let track_id = source_track["id"].as_str().unwrap().to_string();

    let report_card = boot
        .repo
        .cards_by_track(&track_id)
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.kind == "track-report")
        .expect("fresh track has a report card");
    let payload: TrackReportPayload =
        serde_json::from_value(report_card.payload).expect("initial report payload");
    assert_eq!(payload, TrackReportPayload::initial());
    assert!(!payload.report_startup_read_required());

    let (ctx, registry, identity) = planner_tool_channel(&boot, &track_id).await;
    let state = call_planner_tool(&ctx, &registry, TOOL_TRACK_STATE, identity, json!({}))
        .await
        .expect("fresh planner can read track state");
    assert_eq!(
        state
            .get("report_startup_read_required")
            .and_then(Value::as_bool),
        Some(false),
        "canonical initial report must not require a startup read: {state}"
    );
}

// ---------------------------------------------------------------------------
// #1252 S2 — what the structural door may not do
//
// The door (`track_report::write::structural_init_report_tx`) is the create
// paths' entry into the report write boundary. `fork_guard_exemption_invariant`
// pins its *signature* — that it has no `EventBus`, no `EditAuthor`, no CAS
// input. The first two tests below pin the behaviour that signature exists to
// produce, end to end through `POST /api/tracks`, for **all three** creation
// sources that build an `init_snapshot`: a fork, a built-in template
// instantiation, and a user recipe (`TrackInit::Recipe`, #1292 S2 — see
// `routes::tracks`'s own "three initialization sources"). All three reach the
// same door, so a change that gave it an event bus would break all three, and
// testing a subset would leave part of the door uncovered — which is exactly
// what happened between #1252 S2 and #1292 S2 landing: this comment said "both"
// and the recipe arm was created afterwards, with no case here.
//
// The third is narrower than its neighbours and says so in its own doc comment:
// the order of the door's two statements is **not** observable from this
// surface, and the test that pins it lives in the `--lib` target.
// ---------------------------------------------------------------------------

/// Every persisted event of `kind` scoped to `track_id`, oldest first, as
/// `(actor, payload)` raw JSON.
///
/// Read out of the `events` table rather than off a broadcast channel: the
/// persisted row is what the audit log, the goldens and replay all consume, and
/// an event that is emitted but not persisted is not the thing being denied
/// here.
async fn events_for_track(repo: &dyn Repo, kind: &str, track_id: &str) -> Vec<(Value, Value)> {
    let kind = kind.to_string();
    let track_id = track_id.to_string();
    let rows: Vec<(String, String)> = calm_server::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            Ok(sqlx::query_as(
                "SELECT actor,payload FROM events WHERE kind=?1 AND scope_track=?2 ORDER BY id ASC",
            )
            .bind(kind)
            .bind(track_id)
            .fetch_all(&mut **tx)
            .await?)
        })
    })
    .await
    .unwrap();
    rows.into_iter()
        .map(|(actor, payload)| {
            (
                serde_json::from_str(&actor).expect("events.actor is JSON"),
                serde_json::from_str(&payload).expect("events.payload is JSON"),
            )
        })
        .collect()
}

/// Create a track through the production REST route and return its id.
async fn create_track_via_rest(boot: &Boot, title: &str, suffix: &str, extra: Value) -> String {
    let mut body = json!({
        "area_id": boot.area_id,
        "title": title,
        "sort": null,
        "cwd": target_cwd(suffix),
        "attach_folder": true,
        "theme": routes::theme::RequestTheme::default_dark(),
    });
    for (key, value) in extra.as_object().expect("extra must be an object") {
        body[key] = value.clone();
    }
    let (status, created) = request_json(
        &boot.app,
        "POST",
        "/api/tracks".into(),
        &boot.cookie,
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create `{title}`: {created}");
    created["id"].as_str().unwrap().to_string()
}

/// Create a track recipe through the production REST route and return its id.
///
/// The body is `track_recipe_instantiate`'s own `two_task_body` — the same
/// shape #1292 S2 pins the recipe path with, reused rather than re-invented so
/// that "the recipe path reaches the door" is asserted about the recipe shape
/// that path is actually specified against.
async fn create_recipe_via_rest(boot: &Boot, title: &str) -> String {
    let (status, created) = request_json(
        &boot.app,
        "POST",
        "/api/track-recipes".into(),
        &boot.cookie,
        Some(json!({
            "title": title,
            "body": crate::track_recipe_instantiate::two_task_body(),
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "create recipe `{title}`: {created}"
    );
    created["id"].as_str().unwrap().to_string()
}

/// Issue #1252 S2 / Q12 — **no creation source emits a report edit.**
///
/// A fork, a built-in template instantiation and a user recipe all write the
/// new track's report card
/// inside the create transaction. Doing that through a shared writer is exactly
/// the change that "naturally" unifies them onto `write::persist`, which emits
/// `card.updated` + `track.report_edited` on every successful call — so the
/// thing most likely to be lost in this slice is the *absence* of that pair.
/// Q12 ruled the absence is the contract.
///
/// It is carried by the door's signature (no `&EventBus`, no event in the
/// return type — `fork_guard_exemption_invariant::
/// the_structural_door_cannot_name_an_author_an_actor_or_a_revision`), and by
/// this test end to end. **Must-red**: give the door an `EventBus` and emit a
/// `TrackReportEdited` from it, and this goes red on whichever creation source
/// you wired it into — which is why all three are here.
///
/// Scoped by `scope_track`, so the source track's own seeded edits (the
/// fixture writes those through the real REST/`persist_report` path, which
/// *does* emit) cannot mask a missing assertion: an unscoped count would be
/// non-zero either way.
#[tokio::test]
async fn every_creation_source_emits_no_report_edited() {
    let boot = boot().await;

    let forked = create_track_via_rest(
        &boot,
        "no-report-edited fork",
        "-no-edit-fork",
        json!({ "fork_report_from": boot.source_track_id }),
    )
    .await;
    let templated = create_track_via_rest(
        &boot,
        "no-report-edited template",
        "-no-edit-template",
        json!({ "template_id": "small-change" }),
    )
    .await;
    let recipe_id = create_recipe_via_rest(&boot, "no-report-edited recipe").await;
    let from_recipe = create_track_via_rest(
        &boot,
        "no-report-edited recipe track",
        "-no-edit-recipe",
        json!({ "recipe_id": recipe_id }),
    )
    .await;

    // The fixture: both new tracks really do carry initialized report content,
    // so "no edit event" is a statement about a write that happened rather than
    // about a create that did nothing.
    for track_id in [&forked, &templated, &from_recipe] {
        let (status, report) = request_json(
            &boot.app,
            "GET",
            format!("/api/tracks/{track_id}/report"),
            &boot.cookie,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            !report["blocks"].as_array().unwrap().is_empty(),
            "structural init wrote nothing for {track_id}: {report}"
        );
        assert_eq!(
            report["docRev"], 0,
            "structural init must not advance docRev for {track_id}: {report}"
        );
    }

    for (label, track_id) in [
        ("fork", &forked),
        ("template", &templated),
        ("recipe", &from_recipe),
    ] {
        let edits = events_for_track(boot.repo.as_ref(), "track.report_edited", track_id).await;
        assert!(
            edits.is_empty(),
            "{label} creation emitted a report edit; the structural door has no event bus to \
             emit one with: {edits:#?}"
        );
    }
}

/// Issue #1252 S2 / Q12, second half — the report card's **only** event is one
/// `card.added`, attributed to the request's own actor.
///
/// `every_creation_source_emits_no_report_edited` cannot see this: unifying
/// the create paths onto `write::persist` would emit `card.updated` *and*
/// `track.report_edited`, but so would a narrower mistake that emits only the
/// generic one. This is that leg. It also pins the attribution, which is where
/// the fork's "who" survives at all — the door emits nothing, so the create
/// closure's `CardAdded` is the single record that a user did this.
///
/// **Must-red**: make the door emit `Event::CardUpdated` for the row it writes.
#[tokio::test]
async fn structural_init_leaves_one_card_added_and_no_card_updated() {
    let boot = boot().await;
    let recipe_id = create_recipe_via_rest(&boot, "one card.added recipe").await;

    for (label, suffix, extra) in [
        (
            "fork",
            "-one-card-added-fork",
            json!({ "fork_report_from": boot.source_track_id }),
        ),
        (
            "template",
            "-one-card-added-template",
            json!({ "template_id": "small-change" }),
        ),
        (
            "recipe",
            "-one-card-added-recipe",
            json!({ "recipe_id": recipe_id }),
        ),
    ] {
        let track_id =
            create_track_via_rest(&boot, &format!("one card.added {label}"), suffix, extra).await;
        let report_card = boot
            .repo
            .cards_by_track(&track_id)
            .await
            .unwrap()
            .into_iter()
            .find(|card| card.kind == "track-report")
            .expect("created track has a report card");

        let added: Vec<(Value, Value)> =
            events_for_track(boot.repo.as_ref(), "card.added", &track_id)
                .await
                .into_iter()
                .filter(|(_, payload)| payload["id"] == json!(report_card.id.as_str()))
                .collect();
        assert_eq!(
            added.len(),
            1,
            "{label}: the report card must be announced exactly once: {added:#?}"
        );
        // `ActorId::User` is what `Actor::to_actor_id()` produces for this
        // request; the fixture sends no `X-Calm-Actor`, which is the browser
        // shape. Compared as the serialized `ActorId` the events table stores.
        assert_eq!(
            added[0].0,
            serde_json::to_value(calm_server::ids::ActorId::User).unwrap(),
            "{label}: the report card's `card.added` carries the request's actor"
        );

        let updated: Vec<(Value, Value)> =
            events_for_track(boot.repo.as_ref(), "card.updated", &track_id)
                .await
                .into_iter()
                .filter(|(_, payload)| payload["id"] == json!(report_card.id.as_str()))
                .collect();
        assert!(
            updated.is_empty(),
            "{label}: the structural door must not emit `card.updated` for the row it \
             initializes: {updated:#?}"
        );
    }
}

/// Issue #1252 S2 — a forked task's `refs` are rewritten onto the copy and
/// resolve against it.
///
/// # What this does NOT pin, and how that was established
///
/// It does not pin the statement order inside
/// `track_report::write::write_report_row_and_project_tx`, and an earlier draft
/// of this test claimed it did. **Measured, not argued**: swapping the row write
/// and the task projection inside that function leaves this test GREEN. The
/// reason is that `GET /api/tracks/{id}/report` recomputes `taskDiagnostics` at
/// read time from the committed payload, so by the time this test looks, the
/// cache holds the fork either way — the create-time projection's verdicts are
/// simply not on this surface.
///
/// Nor is the difference visible in the `tasks` table: `prepare_fork_report`
/// forces every copied task to `ready: false`, so a forked declaration is
/// non-schedulable and projects no row whichever order ran. That is why the
/// fork route had no order test before this slice, and it is not something a
/// better assertion here would fix.
///
/// The test that *does* pin the order is
/// `routes::tracks::tests::structural_door_writes_cache_crdt_and_projection_together`
/// — the crate's `--lib` target, which reads the `TaskProjectionOutcome` the
/// door returns rather than a re-derived read-path value. Under the same swap it
/// goes red on a `reference_missing` diagnostic. Naming it here is the point: a
/// gate run scoped to the integration binaries never builds that target.
///
/// What this test is still worth: the fork's link rewriting and the copy's
/// self-consistency end to end. The fixture's `build` task references the source
/// track's second prose block; after the fork the reference must name the
/// *copy* of that block, on the new track, and that block must be present.
#[tokio::test]
async fn forked_task_refs_are_rewritten_onto_the_copy_and_resolve() {
    let boot = boot().await;
    let (status, source_report) = request_json(
        &boot.app,
        "GET",
        format!("/api/tracks/{}/report", boot.source_track_id),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let internal_block_id = block_index(&source_report)[2].0.clone();

    let target_track_id = create_track_via_rest(
        &boot,
        "ref resolution fork",
        "-ref-resolution",
        json!({ "fork_report_from": boot.source_track_id }),
    )
    .await;

    let (status, report) = request_json(
        &boot.app,
        "GET",
        format!("/api/tracks/{target_track_id}/report"),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The fixture, asserted rather than assumed: the copied task really does
    // reference a block of this same write, rewritten onto the new track.
    let forked = task_block_by_key(&report, "build");
    assert_eq!(
        forked["payload"]["refs"],
        json!([internal_block_ref(&target_track_id, &internal_block_id)]),
        "the forked task must reference the copy of the source block: {forked}"
    );
    assert!(
        report["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|block| block["id"] == json!(internal_block_id)),
        "the referenced block must be part of the same write: {report}"
    );

    let dangling: Vec<&Value> = report["taskDiagnostics"]
        .as_array()
        .expect("taskDiagnostics array")
        .iter()
        .filter(|verdict| {
            verdict["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|diagnostic| diagnostic["code"] == "reference_missing")
        })
        .collect();
    assert!(
        dangling.is_empty(),
        "the fork left a reference that does not resolve against the copy: {dangling:#?}"
    );
}

/// #1252 S3′ — the negative nail.
///
/// #1252 and #1362 both asked for a test of "the S2 × S3′ intersection": once
/// S2 routed fork / template / recipe creation through a unified apply, fork's
/// events were supposed to start flowing through the
/// `append_decision_event*_in_tx` seam.
///
/// **That intersection does not exist.** Fork goes `routes::tracks` →
/// `write_with_actor_events_typed` → `write_with_actor_events` →
/// `enforce_role_resolving_session`, one of the four `RepoEventWrite` wrappers
/// gated since #136 PR3; S2's structured creation door lands on the same
/// wrapper. Neither touches `append_decision_event_in_tx`. Writing the
/// requested test would have meant asserting a fiction, so this pins the true
/// statement — and it is a real regression guard: if a refactor ever reroutes a
/// report/fork write through the seam, this goes red.
///
/// `calm_truth::db::sqlite::append_probe` is a process-global recorder, which
/// is sound only because the gate command runs tests under `cargo nextest`
/// (one process per test).
#[tokio::test]
async fn fork_creation_events_do_not_cross_the_append_decision_seam() {
    use calm_truth::db::sqlite::append_probe;

    /// Kinds that would mean a report/track body write had started arriving at
    /// the seam. `workspace.*` deliberately is not here: track creation leases
    /// a workspace, and the workspace-lease adapter is one of the fifteen
    /// legitimate seam call sites.
    const REPORT_SHAPED_KINDS: &[&str] =
        &["card.updated", "card.added", "track.updated", "track.added"];

    let boot = boot().await;

    // Only the fork request is under observation.
    append_probe::reset();
    let (status, target_track) = request_json(
        &boot.app,
        "POST",
        "/api/tracks".into(),
        &boot.cookie,
        Some(json!({
            "area_id": boot.area_id,
            "title": "seam nail target",
            "sort": null,
            "cwd": target_cwd("-seam-nail"),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_track_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {target_track}");
    let observed = append_probe::kinds();

    // Today `observed` is empty. It is deliberately *not* asserted empty:
    // track creation also leases a workspace, and the workspace-lease adapter
    // is a legitimate seam call site whose events may land in this window. The
    // nail is about report/card-shaped traffic, so it is written as a denylist
    // over kinds rather than as "the seam stayed silent".
    for kind in &observed {
        assert!(
            !REPORT_SHAPED_KINDS.contains(kind),
            "a report/card-shaped event crossed the append_decision seam during a \
             fork: {kind} (full trace: {observed:?}). Fork is supposed to go through \
             write_with_actor_events, which is gated by enforce_role_resolving_session. \
             If it moved on purpose, replace this nail with a positive test of the \
             seam's verdict on fork's actor."
        );
    }

    // The loop above is vacuous unless the probe actually records. Prove it
    // does, in this same process, by driving the seam directly: a `Kernel`
    // actor appending a system-scoped event is exactly the shape seven of the
    // fifteen production call sites use, and it must show up in the trace.
    let side_repo = calm_server::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open probe-liveness repo");
    append_probe::reset();
    let liveness_event = calm_server::event::Event::TaskDispatched {
        idempotency_key: "s3-seam-probe-liveness".into(),
        kind: "codex".into(),
        agent_message: None,
    };
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(side_repo.pool())
        .await
        .expect("begin probe-liveness tx");
    calm_server::db::sqlite::append_decision_event_in_tx(
        &mut tx,
        &ActorId::Kernel,
        &calm_server::event::EventScope::System,
        None,
        &liveness_event,
    )
    .await
    .expect("kernel/system append must be admitted");
    tx.commit().await.expect("commit probe-liveness tx");
    assert_eq!(
        append_probe::kinds(),
        vec!["task.dispatched"],
        "the append probe is not wired, so the fork assertion above proved nothing"
    );
}
