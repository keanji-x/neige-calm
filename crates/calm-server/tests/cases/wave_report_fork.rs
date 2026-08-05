#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{self, AuthConfig, AuthState, SESSION_COOKIE};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{EditAuthor, EventBus};
use calm_server::ids::ActorId;
use calm_server::model::{NewCard, NewCove, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_report::{WaveReportPayload, persist_report};
use calm_types::report_blocks::render_fence;
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
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, cookie);
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

async fn fork_row_counts(repo: &dyn Repo) -> (i64, i64, i64, i64, i64) {
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
            Ok((waves, folders, spec_cards, report_cards, overlays))
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
                "key": "rejected", "tombstone": {"reason": "not now"},
                "declared_by": "spec", "tombstoned_by": "user"
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
