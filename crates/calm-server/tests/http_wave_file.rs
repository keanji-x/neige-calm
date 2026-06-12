//! Issue #573 phase 1 - authenticated HTTP wave file views.
//!
//! These tests compare the new REST endpoints against the existing MCP
//! `calm.wave.ls` / `calm.wave.cat` outputs over the same in-memory repo.

#![cfg(unix)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::mcp_server::tools::wave_file::{TOOL_WAVE_CAT, TOOL_WAVE_LS};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use support::wave_file::{
    app, boot, call_tool, login, materialize_worker, request_codex, spec_identity,
};
use tower::ServiceExt;

async fn get_json(app: &axum::Router, uri: String, cookie: Option<&str>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    let resp = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        panic!(
            "response body must be JSON: {e}; status={status}; body={}",
            String::from_utf8_lossy(&bytes)
        )
    });
    (status, body)
}

fn ls_uri(wave_id: &str, path: Option<&str>) -> String {
    match path {
        Some(path) => format!("/api/waves/{wave_id}/files/ls?path={path}"),
        None => format!("/api/waves/{wave_id}/files/ls"),
    }
}

fn cat_uri(wave_id: &str, path: &str) -> String {
    format!("/api/waves/{wave_id}/files/cat?path={path}")
}

#[tokio::test]
async fn http_ls_and_cat_match_mcp_outputs() {
    let boot = boot().await;
    request_codex(&boot, "run-list").await;
    let run_card_id = materialize_worker(&boot, "run-list").await;
    let app = app(&boot);
    let cookie = login(&app).await;
    let wave_id = boot.wave_id.as_str();
    let card_dir_path = format!("cards/{}", run_card_id.as_str());

    for path in [
        "/".to_string(),
        "cards".to_string(),
        "runs".to_string(),
        card_dir_path,
    ] {
        let mcp = call_tool(
            &boot,
            TOOL_WAVE_LS,
            spec_identity(&boot),
            json!({ "path": path.as_str() }),
        )
        .await
        .unwrap_or_else(|e| panic!("MCP ls {path} failed: {e}"));
        let (status, http) =
            get_json(&app, ls_uri(wave_id, Some(path.as_str())), Some(&cookie)).await;
        assert_eq!(status, StatusCode::OK, "HTTP ls {path}: {http}");
        assert_eq!(http, mcp, "HTTP ls {path} must match MCP");
    }

    let initial_payload_path = format!("cards/{}/.payload.json", boot.worker_card_id.as_str());
    let initial_runtime_path = format!("cards/{}/runtime.json", boot.worker_card_id.as_str());
    let payload_path = format!("cards/{}/.payload.json", run_card_id.as_str());
    let runtime_path = format!("cards/{}/runtime.json", run_card_id.as_str());
    let conversation_path = format!("cards/{}/conversation.md", run_card_id.as_str());
    let cat_paths = vec![
        "index.md".to_string(),
        "wave.json".to_string(),
        "cards/index.json".to_string(),
        "runs/index.json".to_string(),
        "runs/run-list.md".to_string(),
        "runs/run-list.json".to_string(),
        "report.md".to_string(),
        conversation_path,
        initial_payload_path,
        initial_runtime_path,
        payload_path,
        runtime_path,
    ];
    for path in cat_paths {
        let mcp = call_tool(
            &boot,
            TOOL_WAVE_CAT,
            spec_identity(&boot),
            json!({ "path": path }),
        )
        .await
        .unwrap_or_else(|e| panic!("MCP cat {path} failed: {e}"));
        let (status, http) = get_json(&app, cat_uri(wave_id, &path), Some(&cookie)).await;
        assert_eq!(status, StatusCode::OK, "HTTP cat {path}: {http}");
        assert_eq!(http, mcp, "HTTP cat {path} must match MCP");
    }
}

#[tokio::test]
async fn missing_session_returns_401() {
    let boot = boot().await;
    let app = app(&boot);
    let (status, body) = get_json(&app, ls_uri(boot.wave_id.as_str(), Some("/")), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["code"], json!("unauthorized"));
}

#[tokio::test]
async fn missing_wave_returns_404() {
    let boot = boot().await;
    let app = app(&boot);
    let cookie = login(&app).await;
    let (status, body) = get_json(&app, ls_uri("not-a-wave", None), Some(&cookie)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["code"], json!("not_found"));
}

#[tokio::test]
async fn unknown_path_returns_400_with_mcp_message() {
    let boot = boot().await;
    let app = app(&boot);
    let cookie = login(&app).await;
    let (status, body) =
        get_json(&app, cat_uri(boot.wave_id.as_str(), "nope"), Some(&cookie)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], json!("bad_request"));
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("calm.wave: path not available in this view: nope"),
        "{body}"
    );
}

#[tokio::test]
async fn cross_wave_card_path_returns_403() {
    let boot = boot().await;
    let app = app(&boot);
    let cookie = login(&app).await;
    let path = format!("cards/{}/.payload.json", boot.other_wave_card_id.as_str());
    let (status, body) = get_json(&app, cat_uri(boot.wave_id.as_str(), &path), Some(&cookie)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], json!("forbidden"));
    assert!(
        body["error"].as_str().unwrap().contains("forbidden: card"),
        "{body}"
    );
}
