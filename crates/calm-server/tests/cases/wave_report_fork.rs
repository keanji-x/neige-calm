#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{self, AuthConfig, AuthState, SESSION_COOKIE};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_insert_tx, session_mark_wave_root_tx};
use calm_server::error::CalmError;
use calm_server::event::{EditAuthor, EventBus};
use calm_server::ids::{ActorId, CardId, WaveId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::wave_report_blocks::{
    TOOL_REPORT_BLOCKS_DELETE, TOOL_REPORT_BLOCKS_UPSERT,
};
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::session_projection_repo::AgentProvider;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_report::{WaveReportPayload, persist_report};
use calm_types::report_blocks::render_fence;
use calm_types::worker::{
    LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession, WorkerSessionId,
    WorkerSessionState,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    state: AppState,
    repo: Arc<dyn Repo>,
    cove_id: String,
    other_cove_id: String,
    source_wave_id: String,
    source_report_id: String,
    target_cwd: String,
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

fn fixture_prose(source_wave_id: &str, internal_block_id: &str) -> String {
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
            "[external](neige://wave/external-wave#b_4444)\n",
            "\n[same]: neige://wave/{0}#{1}\n",
        ),
        source_wave_id,
        internal_block_id,
        long = long_fixture_text(),
    )
}

fn primary_task_payload(source_wave_id: &str, internal_block_id: &str) -> Value {
    json!({
        "key": "build",
        "kind": "codex",
        "goal": format!("Goal [internal](neige://wave/{source_wave_id}#{internal_block_id})"),
        "acceptance": format!("Accept <neige://wave/{source_wave_id}#{internal_block_id}>") ,
        "refs": [format!("neige://wave/{source_wave_id}#{internal_block_id}")],
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
    let cove = repo
        .cove_create(NewCove {
            name: "fork-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let source = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "source".into(),
            sort: None,
            cwd: tmp.path().to_string_lossy().into_owned(),
            workflow_id: None,
            workflow_input: None,
            attach_folder: false,
            theme: routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let other_cove = repo
        .cove_create(NewCove {
            name: "fork-test-other".into(),
            color: "#111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let report = repo
        .card_create(NewCard {
            wave_id: source.id.clone(),
            kind: "wave-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(WaveReportPayload::initial()).unwrap(),
            title: None,
        })
        .await
        .unwrap();

    let events = EventBus::new();
    let card_roles = CardRoleCache::new();
    let wave_coves = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_coves).await.unwrap();
    let write = calm_server::state::WriteContext::new(card_roles.clone(), wave_coves.clone());
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
            codex.codex_bin = "/nonexistent-codex-bin-wave-report-fork".into();
            Arc::new(codex)
        },
        Some(card_roles),
        Some(wave_coves),
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
        WaveReportPayload::initial(),
        WaveReportPayload::new("fork source summary", seed_fixture_body()),
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
        format!("/api/waves/{source_id}/report"),
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
            "/api/waves/{source_id}/report/blocks/{}",
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
            "/api/waves/{source_id}/report/blocks/{}",
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
        cove_id: cove.id.to_string(),
        other_cove_id: other_cove.id.to_string(),
        source_wave_id: source_id,
        source_report_id: report.id.to_string(),
        target_cwd: tmp.path().join("target").to_string_lossy().into_owned(),
        cookie,
        _tmp: tmp,
    }
}

#[tokio::test]
async fn fork_source_and_cove_checks_fail_with_four_hundred_and_roll_back_creation() {
    let boot = boot().await;
    let (status, body) = request_json(
        &boot.app,
        "POST",
        "/api/waves".into(),
        &boot.cookie,
        Some(json!({
            "cove_id": boot.other_cove_id,
            "title": "cross-cove target",
            "sort": null,
            "cwd": format!("{}-cross", boot.target_cwd),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_wave_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body = {body}");
    assert!(
        boot.repo
            .waves_by_cove(&boot.other_cove_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        boot.repo
            .cove_folders_by_cove(&boot.other_cove_id)
            .await
            .unwrap()
            .is_empty()
    );

    let source_cove_id = boot.cove_id.clone();
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            sqlx::query("UPDATE coves SET kind='system' WHERE id=?1")
                .bind(source_cove_id)
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
        "/api/waves".into(),
        &boot.cookie,
        Some(json!({
            "cove_id": boot.other_cove_id,
            "title": "system-source target",
            "sort": null,
            "cwd": format!("{}-system", boot.target_cwd),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_wave_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "system source body = {body}");
    assert_eq!(
        boot.repo
            .waves_by_cove(&boot.other_cove_id)
            .await
            .unwrap()
            .len(),
        1,
        "a system-cove source may seed a wave in another cove"
    );

    let waves_before = boot.repo.waves_by_cove(&boot.cove_id).await.unwrap().len();
    let (status, body) = request_json(
        &boot.app,
        "POST",
        "/api/waves".into(),
        &boot.cookie,
        Some(json!({
            "cove_id": boot.cove_id,
            "title": "missing-source target",
            "sort": null,
            "cwd": format!("{}-missing", boot.target_cwd),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": "wave-does-not-exist",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body = {body}");
    assert_eq!(
        boot.repo.waves_by_cove(&boot.cove_id).await.unwrap().len(),
        waves_before,
        "failed fork must roll back the new wave row"
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
/// assertion covers the whole surface: the wave row, the attached cove folder,
/// both cards (spec + reportcard), the overlays, and the `tasks` rows that
/// `persist_fork_report_and_project_tasks_tx` projects out of the copied report.
async fn fork_row_counts(repo: &dyn Repo) -> (i64, i64, i64, i64, i64, i64) {
    calm_server::db::write_in_tx_typed(repo, |tx| {
        Box::pin(async move {
            let waves = sqlx::query_scalar("SELECT COUNT(*) FROM waves")
                .fetch_one(&mut **tx)
                .await?;
            let folders = sqlx::query_scalar("SELECT COUNT(*) FROM cove_folders")
                .fetch_one(&mut **tx)
                .await?;
            let spec_cards = sqlx::query_scalar("SELECT COUNT(*) FROM cards WHERE role='spec'")
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
            Ok((waves, folders, spec_cards, report_cards, overlays, tasks))
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
        format!("/api/waves/{}/report", boot.source_wave_id),
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

    let (status, target_wave) = request_json(
        &boot.app,
        "POST",
        "/api/waves".into(),
        &boot.cookie,
        Some(json!({
            "cove_id": boot.cove_id,
            "title": "fork target",
            "sort": null,
            "cwd": boot.target_cwd,
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_wave_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {target_wave}");
    let target_wave_id = target_wave["id"].as_str().unwrap().to_string();

    let (status, target_report) = request_json(
        &boot.app,
        "GET",
        format!("/api/waves/{target_wave_id}/report"),
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
            "[external](neige://wave/external-wave#b_4444)\n",
            "\n[same]: neige://wave/{0}#{1}\n",
        ),
        target_wave_id,
        internal_block_id,
        boot.source_wave_id,
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
                "goal": format!("Goal [internal](neige://wave/{target_wave_id}#{internal_block_id})"),
                "acceptance": format!("Accept <neige://wave/{target_wave_id}#{internal_block_id}>") ,
                "refs": [format!("neige://wave/{target_wave_id}#{internal_block_id}")],
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
                // #1111 — the copy is spec-owned on BOTH privilege fields.
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
        .cards_by_wave(&target_wave_id)
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.kind == "wave-report")
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
        format!("/api/waves/{target_wave_id}/report"),
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
        format!("/api/waves/{}/report", boot.source_wave_id),
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
    // that schema; constructing this through today's WaveReportPayload would
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
        format!("/api/waves/{}/report", boot.source_wave_id),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let source_truth = block_index(&source_report);
    assert_eq!(source_truth.len(), 1);
    assert_eq!(source_report["schemaVersion"], 1);

    let (status, target_wave) = request_json(
        &boot.app,
        "POST",
        "/api/waves".into(),
        &boot.cookie,
        Some(json!({
            "cove_id": boot.cove_id,
            "title": "legacy fork target",
            "sort": null,
            "cwd": format!("{}-legacy", boot.target_cwd),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_wave_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {target_wave}");
    let target_id = target_wave["id"].as_str().unwrap();
    let (status, target_report) = request_json(
        &boot.app,
        "GET",
        format!("/api/waves/{target_id}/report"),
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
    let (status, source_wave) = request_json(
        &boot.app,
        "POST",
        "/api/waves".into(),
        &boot.cookie,
        Some(json!({
            "cove_id": boot.cove_id,
            "title": "canonical fresh source",
            "sort": null,
            "cwd": format!("{}-canonical-source", boot.target_cwd),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {source_wave}");
    let source_id = source_wave["id"].as_str().unwrap();
    let source_card = boot
        .repo
        .cards_by_wave(source_id)
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.kind == "wave-report")
        .unwrap();
    let raw_source = boot
        .repo
        .card_get_with_body_crdt(source_card.id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        raw_source.0.payload,
        serde_json::to_value(WaveReportPayload::initial()).unwrap()
    );
    assert!(raw_source.1.is_none(), "fresh report unexpectedly had CRDT");

    let (status, source_report) = request_json(
        &boot.app,
        "GET",
        format!("/api/waves/{source_id}/report"),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, target_wave) = request_json(
        &boot.app,
        "POST",
        "/api/waves".into(),
        &boot.cookie,
        Some(json!({
            "cove_id": boot.cove_id,
            "title": "canonical fresh target",
            "sort": null,
            "cwd": format!("{}-canonical-target", boot.target_cwd),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": source_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {target_wave}");
    let target_id = target_wave["id"].as_str().unwrap();
    let (status, target_report) = request_json(
        &boot.app,
        "GET",
        format!("/api/waves/{target_id}/report"),
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
    let (status, source_wave) = request_json(
        &boot.app,
        "POST",
        "/api/waves".into(),
        &boot.cookie,
        Some(json!({
            "cove_id": boot.cove_id,
            "title": "empty source",
            "sort": null,
            "cwd": format!("{}-empty-source", boot.target_cwd),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {source_wave}");
    let source_id = source_wave["id"].as_str().unwrap();
    let (status, fresh_report) = request_json(
        &boot.app,
        "GET",
        format!("/api/waves/{source_id}/report"),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let seed = &fresh_report["blocks"][0];
    let (status, empty_write) = request_json(
        &boot.app,
        "DELETE",
        format!(
            "/api/waves/{source_id}/report/blocks/{}",
            seed["id"].as_str().unwrap()
        ),
        &boot.cookie,
        Some(json!({"ifBlockRev": seed["rev"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "empty report write: {empty_write}");
    let (status, source_report) = request_json(
        &boot.app,
        "GET",
        format!("/api/waves/{source_id}/report"),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(source_report["blocks"], json!([]));

    let (status, target_wave) = request_json(
        &boot.app,
        "POST",
        "/api/waves".into(),
        &boot.cookie,
        Some(json!({
            "cove_id": boot.cove_id,
            "title": "empty target",
            "sort": null,
            "cwd": format!("{}-empty-target", boot.target_cwd),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": source_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {target_wave}");
    let target_id = target_wave["id"].as_str().unwrap();
    let target_card = boot
        .repo
        .cards_by_wave(target_id)
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.kind == "wave-report")
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
    let target_doc = calm_server::wave_report_doc::ReportDoc::from_bytes(
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
        Vec::<calm_types::wave_report::ReportBlock>::new()
    );
}

#[tokio::test]
async fn invalid_fork_payload_rest_path_rolls_back_every_created_row() {
    let boot = boot().await;
    let invalid_body = render_fence("app", &json!({"src": "https://example.invalid/app"}));
    let invalid_payload = WaveReportPayload::new("invalid fork source", invalid_body);
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
        "/api/waves".into(),
        &boot.cookie,
        Some(json!({
            "cove_id": boot.cove_id,
            "title": "invalid payload target",
            "sort": null,
            "cwd": format!("{}-invalid-payload", boot.target_cwd),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_wave_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body = {body}");
    assert!(body.to_string().contains("src: required same-origin path"));
    let after = fork_row_counts(boot.repo.as_ref()).await;
    assert_eq!(after.0, before.0, "failed fork left a waves row");
    assert_eq!(after.1, before.1, "failed fork left a cove_folders row");
    assert_eq!(after.2, before.2, "failed fork left a spec card");
    assert_eq!(after.3, before.3, "failed fork left a report card");
    assert_eq!(after.4, before.4, "failed fork left an overlay");
    assert_eq!(after.5, before.5, "failed fork left a tasks row");
}

/// Issue #1111 — the companion half of the tombstone normalization: fork
/// rewrites `tombstoned_by` **only** on tombstone blocks. A residual
/// `tombstoned_by` on a *non*-tombstone task is deliberately left alone so the
/// fork's own `validate_payload` breaks the whole wave creation fail-closed,
/// rather than silently repairing a corrupt source into a shape it never
/// validly had.
///
/// **This shape is unreachable in production — this is not a realistic
/// regression.** Every write surface that can produce a stored report runs
/// `validate_payload` first: the whole-document path via
/// `wave_report.rs:392,409 → wave_report_guard.rs:38`, and the block-level
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
    let residual_payload = WaveReportPayload::new("residual tombstoned_by source", residual_body);
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
        "/api/waves".into(),
        &boot.cookie,
        Some(json!({
            "cove_id": boot.cove_id,
            "title": "residual tombstoned_by target",
            "sort": null,
            "cwd": format!("{}-residual-tombstoned-by", boot.target_cwd),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_wave_id,
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
    assert_eq!(after.0, before.0, "failed fork left a waves row");
    assert_eq!(after.1, before.1, "failed fork left a cove_folders row");
    assert_eq!(after.2, before.2, "failed fork left a spec card");
    assert_eq!(after.3, before.3, "failed fork left a report card");
    assert_eq!(after.4, before.4, "failed fork left an overlay");
    assert_eq!(after.5, before.5, "failed fork left a tasks row");
}

/// Issue #1115 — the other half of the release normalization: a template that
/// holds a user-released task is still forkable, and the fork lands without the
/// release.
///
/// This test used to be `non_user_fork_rejects_user_released_task_and_rolls_
/// back_every_created_row` and pinned the opposite outcome (400 + zero
/// residue). That outcome was not a contract: it was the observable face of the
/// same missing normalization. `prepare_fork_report` left `released_by_user`
/// alone, so a template that any user had ever released became **un-forkable**
/// — Rule 5 rejected the whole wave creation, with no repair path short of a
/// human editing the template. Now the flag is normalized away before
/// `guard_forked_blocks` ever sees it, so nothing is left for Rule 5 to reject.
///
/// The privilege that the old 400 nominally protected is not weakened: no
/// forked block reaches the new wave carrying a release, whoever forks it.
///
/// **This test does not claim to cover a non-`User` identity, and the name no
/// longer says it does.** It sends `X-Calm-Actor: ai:claude`, but
/// `actor.rs:106-110`'s defensive default maps every header value other than
/// `user` / `ai:codex` onto `ActorId::User` — at the identity layer this
/// request *is* a `User`. What the test actually pins is the fork contract:
/// **a template carrying a release is no longer rejected wholesale, and the
/// copy arrives without the release.**
///
/// A truly non-`User` identity is not reachable on this route at all, which is
/// why no variant of this test asserts one. `ai:codex` is the only header form
/// that maps onto a real AI `ActorId` (`actor.rs:98-104`,
/// `ActorId::AiCodex(CardId::from(""))`), and it carries an empty card id, so
/// `calm-truth/src/role_gate.rs:197` answers `EmptyAiCardId` → 403 when the
/// wave-create event
/// emits. Under `ai:codex` this route cannot reach 201 whatever the report
/// holds — the old 400 was simply an earlier stop on a request that was going
/// to 403 anyway.
#[tokio::test]
async fn fork_of_a_released_template_succeeds_and_drops_the_release() {
    let boot = boot().await;
    seed_user_released_task(&boot, "allowed-upstream").await;

    let (status, target_wave) = request_json_as(
        &boot.app,
        "POST",
        "/api/waves".into(),
        &boot.cookie,
        Some(json!({
            "cove_id": boot.cove_id,
            "title": "released template fork",
            "sort": null,
            "cwd": format!("{}-released-template-fork", boot.target_cwd),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_wave_id,
        })),
        Some("ai:claude"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {target_wave}");
    let target_wave_id = target_wave["id"].as_str().unwrap().to_string();

    let (status, report) = request_json(
        &boot.app,
        "GET",
        format!("/api/waves/{target_wave_id}/report"),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let forked = task_block_by_key(&report, "allowed-upstream");
    assert_ne!(
        forked["payload"]["released_by_user"],
        json!(true),
        "a fork of a released template must not import the release: {forked}"
    );
}

#[tokio::test]
async fn unsafe_markdown_destinations_fail_fork_with_block_and_source() {
    let boot = boot().await;
    let (status, report) = request_json(
        &boot.app,
        "GET",
        format!("/api/waves/{}/report", boot.source_wave_id),
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
        entity_encode_first(&boot.source_wave_id)
    );
    let escaped_destination = format!("neige\\://wave/{}#{target_block_id}", boot.source_wave_id);
    let html_destination = format!("neige://wave/{}#{target_block_id}", boot.source_wave_id);
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
            format!("/api/waves/{}/report", boot.source_wave_id),
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
                "/api/waves/{}/report/blocks/{prose_id}",
                boot.source_wave_id
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
            "/api/waves".into(),
            &boot.cookie,
            Some(json!({
                "cove_id": boot.cove_id,
                "title": format!("unsafe fork {index}"),
                "sort": null,
                "cwd": format!("{}-unsafe-{index}", boot.target_cwd),
                "attach_folder": true,
                "theme": routes::theme::RequestTheme::default_dark(),
                "fork_report_from": boot.source_wave_id,
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
/// `tombstoned_by: "user"` privilege into every wave forked from it.
///
/// The fixture report holds a tombstone declared AND tombstoned by the user.
/// After a fork, the spec author owns the copy: the guard's `user_owned`
/// disjunction (`declared_by == "user" || tombstoned_by == "user"`) plus the
/// immutability of `tombstoned_by` would otherwise freeze that block forever.
#[tokio::test]
async fn forked_user_tombstone_is_normalized_to_spec_and_stays_spec_editable() {
    let boot = boot().await;
    let (status, target_wave) = request_json(
        &boot.app,
        "POST",
        "/api/waves".into(),
        &boot.cookie,
        Some(json!({
            "cove_id": boot.cove_id,
            "title": "tombstone fork target",
            "sort": null,
            "cwd": format!("{}-tombstone", boot.target_cwd),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_wave_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {target_wave}");
    let target_wave_id = target_wave["id"].as_str().unwrap().to_string();

    let (status, target_report) = request_json(
        &boot.app,
        "GET",
        format!("/api/waves/{target_wave_id}/report"),
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

    // Drive the real spec write path (MCP `calm.report.blocks.*` →
    // `CardDecisionSink::commit_report_op` → `guard_task_declarations`).
    let (ctx, registry, identity) = spec_tool_channel(&boot, &target_wave_id).await;
    let rewritten = json!({
        "key": "rejected",
        "tombstone": { "reason": "spec re-scoped this task" },
        "declared_by": tombstone["payload"]["declared_by"].clone(),
        "tombstoned_by": tombstone["payload"]["tombstoned_by"].clone()
    });
    let upsert = call_spec_tool(
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
    .expect("spec author must be able to rewrite a forked tombstone");

    let delete = call_spec_tool(
        &ctx,
        &registry,
        TOOL_REPORT_BLOCKS_DELETE,
        identity,
        json!({ "id": tombstone["id"], "if_rev": upsert["rev"] }),
    )
    .await
    .expect("spec author must be able to delete a forked tombstone");
    assert!(delete.get("docRev").is_some(), "delete returns docRev");

    let (status, after) = request_json(
        &boot.app,
        "GET",
        format!("/api/waves/{target_wave_id}/report"),
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
        "the spec-owned tombstone is gone after the spec delete: {after}"
    );
    assert_eq!(
        tombstone["payload"]["tombstoned_by"], "spec",
        "fork must re-attribute the copied tombstone to the spec author: {tombstone}"
    );
}

const FORKED_SPEC_SESSION_ID: &str = "forked-spec-session";

fn planner_session(id: &str, wave_id: WaveId, card_id: CardId) -> WorkerSession {
    WorkerSession {
        id: WorkerSessionId::from(id),
        wave_id,
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

/// An MCP tool channel bound to the forked wave's own spec card — the
/// production identity a spec agent writes its wave report through.
async fn spec_tool_channel(
    boot: &Boot,
    wave_id: &str,
) -> (Arc<AppContext>, Arc<ToolRegistry>, ToolCallIdentity) {
    let spec_card = boot
        .repo
        .cards_by_wave(wave_id)
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.kind == "codex")
        .expect("forked wave has a spec card");
    let session = planner_session(
        FORKED_SPEC_SESSION_ID,
        WaveId::from(wave_id.to_string()),
        spec_card.id.clone(),
    );
    let root_session_id = session.id.clone();
    let root_wave_id = WaveId::from(wave_id.to_string());
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            session_insert_tx(tx, session)
                .await
                .map_err(CalmError::from)?;
            session_mark_wave_root_tx(tx, &root_wave_id, &root_session_id)
                .await
                .map_err(CalmError::from)?;
            Ok(())
        })
    })
    .await
    .expect("seed the forked wave's root spec session");

    let route_repo: Arc<dyn calm_server::db::RouteRepo> = boot.repo.clone();
    let ctx = Arc::new(AppContext {
        repo: route_repo,
        wave_vcs: None,
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
        card_id: spec_card.id.as_str().to_string(),
        role: CardRole::Spec,
        provider: AgentProvider::Codex,
        session_id: FORKED_SPEC_SESSION_ID.to_string(),
        wave_id: Some(wave_id.to_string()),
        cove_id: boot.cove_id.clone(),
        thread_id: "forked-spec-thread".to_string(),
    };
    (ctx, Arc::new(registry), identity)
}

async fn call_spec_tool(
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

/// The task payload the source wave's user declares. `declared_by: "user"` is
/// the only shape the REST user path may create (Rule 1); fork rewrites it to
/// `"spec"`, which is exactly the shape `declare_and_wait` is meant to hold.
fn released_fixture_payload(key: &str) -> Value {
    json!({
        "key": key,
        "kind": "codex",
        "goal": "Template task the source wave's user allowed",
        "acceptance": "A wave forked from this template must ask its own user again",
        "refs": [],
        // Kept gate-clean on purpose: `declare_and_wait` must be the ONLY
        // diagnostic this fixture can produce, so `schedulable` below is a
        // signal about the release and not about a missing gate.
        "no_gate_reason": "fixture task carries no gate",
        "ready": true,
        "declared_by": "user"
    })
}

/// Seed a live task that the SOURCE wave's user declared and then released,
/// through the production REST user path: `POST .../report/blocks` followed by
/// the same `PATCH` the "Allow this task" button issues
/// (`WaveReportPage.tsx:95-99` spreads the payload and sets
/// `released_by_user: true`). Returns the seeded block id.
async fn seed_user_released_task(boot: &Boot, key: &str) -> String {
    let (status, report) = request_json(
        &boot.app,
        "GET",
        format!("/api/waves/{}/report", boot.source_wave_id),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "read source report: {report}");
    let (status, created) = request_json(
        &boot.app,
        "POST",
        format!("/api/waves/{}/report/blocks", boot.source_wave_id),
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
            "/api/waves/{}/report/blocks/{block_id}",
            boot.source_wave_id
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
/// SOURCE user's consent into a wave forked from it.
///
/// Fork rewrites `declared_by` to `"spec"` (§7.2), which is precisely the shape
/// `declare_and_wait` exists to hold back
/// (`task_projection.rs:709-719`: `effective_wait && declared_by == "spec" &&
/// !released_by_user && !tombstone`). Copying the release flag verbatim exempts
/// the copy from a decision the new wave's user never made — and
/// `report-blocks/task.tsx:185` then hides the "Allow this task" button, so she
/// cannot even see the exemption.
///
/// Whole flow through production REST: seed + release in the source, fork as the
/// browser does (no `X-Calm-Actor`), tighten the new wave to `declare-and-wait`,
/// then let the new user mark the copy ready — the only remaining gate must be
/// her own release.
#[tokio::test]
async fn forked_task_does_not_inherit_the_source_users_release() {
    let boot = boot().await;
    seed_user_released_task(&boot, "allowed-upstream").await;

    let (status, target_wave) = request_json(
        &boot.app,
        "POST",
        "/api/waves".into(),
        &boot.cookie,
        Some(json!({
            "cove_id": boot.cove_id,
            "title": "release normalization target",
            "sort": null,
            "cwd": format!("{}-released", boot.target_cwd),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_wave_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body = {target_wave}");
    let target_wave_id = target_wave["id"].as_str().unwrap().to_string();

    let (status, body) = request_json(
        &boot.app,
        "PATCH",
        format!("/api/waves/{target_wave_id}"),
        &boot.cookie,
        Some(json!({ "automation_policy": "declare-and-wait" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "tighten policy: {body}");

    let (status, report) = request_json(
        &boot.app,
        "GET",
        format!("/api/waves/{target_wave_id}/report"),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let forked = task_block_by_key(&report, "allowed-upstream").clone();
    assert_eq!(
        forked["payload"]["declared_by"], "spec",
        "fork re-attributes the copy to the spec author: {forked}"
    );
    assert_ne!(
        forked["payload"]["released_by_user"],
        json!(true),
        "fork must not carry the source user's release into the new wave: {forked}"
    );

    // The new wave's user marks the copy ready — the ONLY thing that may still
    // hold it back is her own release, so `schedulable` is a live signal here
    // rather than a by-product of the forced `ready: false`.
    let mut ready = forked["payload"].clone();
    ready["ready"] = json!(true);
    let (status, body) = request_json(
        &boot.app,
        "PATCH",
        format!(
            "/api/waves/{target_wave_id}/report/blocks/{}",
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
        format!("/api/waves/{target_wave_id}/report"),
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
        "the forked copy must wait for the NEW wave's user: {verdict}"
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
/// (#1111) — the same deliberate choice: a corrupt source aborts wave creation
/// instead of being silently repaired into a shape it never validly had.
///
/// **This shape is unreachable in production — not a realistic regression.**
/// Every stored-report writer runs `validate_payload` first (whole-document via
/// `wave_report_guard.rs`, block-level via `report_blocks/mod.rs`), and the
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
    let residual_payload = WaveReportPayload::new("released tombstone source", residual_body);
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
        "/api/waves".into(),
        &boot.cookie,
        Some(json!({
            "cove_id": boot.cove_id,
            "title": "released tombstone target",
            "sort": null,
            "cwd": format!("{}-released-tombstone", boot.target_cwd),
            "attach_folder": true,
            "theme": routes::theme::RequestTheme::default_dark(),
            "fork_report_from": boot.source_wave_id,
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
    assert_eq!(after.0, before.0, "failed fork left a waves row");
    assert_eq!(after.1, before.1, "failed fork left a cove_folders row");
    assert_eq!(after.2, before.2, "failed fork left a spec card");
    assert_eq!(after.3, before.3, "failed fork left a report card");
    assert_eq!(after.4, before.4, "failed fork left an overlay");
    assert_eq!(after.5, before.5, "failed fork left a tasks row");
}
