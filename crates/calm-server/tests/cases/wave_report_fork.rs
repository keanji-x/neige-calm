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
    let prose = format!(
        concat!(
            "[inline](neige://wave/{0}#b_1f3a)\n",
            "[reference][same]\n",
            "<neige://wave/{0}#b_ab12>\n",
            "`[code](neige://wave/{0}#b_2222)`\n",
            "```markdown\n[fenced](neige://wave/{0}#b_3333)\n```\n",
            "[external](neige://wave/external-wave#b_4444)\n",
            "\n[same]: neige://wave/{0}#b_5e6f\n",
        ),
        source_id
    );
    let task = json!({
        "key": "build",
        "kind": "codex",
        "goal": format!("Goal [internal](neige://wave/{source_id}#b_1f3a) [external](neige://wave/external-wave#b_4444)"),
        "acceptance": format!("Accept <neige://wave/{source_id}#b_ab12>"),
        "refs": [
            format!("neige://wave/{source_id}#b_1f3a"),
            "neige://wave/external-wave#b_4444"
        ],
        "ready": true,
        "declared_by": "user"
    });
    let tombstone = json!({
        "key": "rejected",
        "tombstone": { "reason": "not now" },
        "declared_by": "user",
        "tombstoned_by": "user"
    });
    let body = format!(
        "{prose}{}{}",
        render_fence("task", &task),
        render_fence("task", &tombstone)
    );
    persist_report(
        repo.as_ref(),
        &state.events,
        state.write(),
        ActorId::User,
        EditAuthor::User,
        source.clone(),
        report.clone(),
        WaveReportPayload::initial(),
        WaveReportPayload::new("fork source", body),
        0,
        None,
        None,
        false,
    )
    .await
    .unwrap();

    let cookie = login(&app).await;
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
    assert_eq!(source_truth.len(), 3);

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

    let blocks = target_report["blocks"].as_array().unwrap();
    let prose = blocks
        .iter()
        .find(|block| block["kind"] == "prose")
        .unwrap();
    let markdown = prose["payload"]["markdown"].as_str().unwrap();
    assert!(markdown.contains(&format!("[inline](neige://wave/{target_wave_id}#b_1f3a)")));
    assert!(markdown.contains(&format!("[same]: neige://wave/{target_wave_id}#b_5e6f")));
    assert!(markdown.contains(&format!("<neige://wave/{target_wave_id}#b_ab12>")));
    assert!(markdown.contains(&format!(
        "`[code](neige://wave/{}#b_2222)`",
        boot.source_wave_id
    )));
    assert!(markdown.contains(&format!(
        "[fenced](neige://wave/{}#b_3333)",
        boot.source_wave_id
    )));
    assert!(markdown.contains("[external](neige://wave/external-wave#b_4444)"));

    let live = blocks
        .iter()
        .find(|block| block["payload"]["key"] == "build")
        .unwrap();
    assert_eq!(live["payload"]["ready"], false);
    assert_eq!(live["payload"]["declared_by"], "spec");
    assert!(
        live["payload"]["goal"]
            .as_str()
            .unwrap()
            .contains(&format!("neige://wave/{target_wave_id}#b_1f3a"))
    );
    assert!(
        live["payload"]["goal"]
            .as_str()
            .unwrap()
            .contains("neige://wave/external-wave#b_4444")
    );
    assert_eq!(
        live["payload"]["refs"],
        json!([
            format!("neige://wave/{target_wave_id}#b_1f3a"),
            "neige://wave/external-wave#b_4444"
        ])
    );
    let tombstone = blocks
        .iter()
        .find(|block| block["payload"]["key"] == "rejected")
        .unwrap();
    assert_eq!(tombstone["payload"]["declared_by"], "spec");
    assert!(tombstone["payload"].get("ready").is_none());

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
    assert_eq!(
        block_index(&crdt_report),
        source_truth,
        "CRDT fallback must preserve the source-captured (id, rev) pairs"
    );

    // Keep fixture fields load-bearing: the source card stayed untouched and
    // the app state remained alive through the post-create operation wait.
    assert!(
        boot.repo
            .card_get(&boot.source_report_id)
            .await
            .unwrap()
            .is_some()
    );
    let _ = &boot.state;
}
