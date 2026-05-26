//! Issue #339 PR A — read-only wave file MCP tools.
//!
//! Drives `calm.wave.ls` / `calm.wave.cat` through the default registry
//! against an in-memory repo. The tools derive scope from the
//! connection-bound `CardIdentity`; none of the calls accepts a wave id.

#![cfg(unix)]

use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::ids::{CardId, WaveId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::wave_file::{TOOL_WAVE_CAT, TOOL_WAVE_LS};
use calm_server::mcp_server::tools::wave_report::TOOL_REPORT_READ;
use calm_server::mcp_server::{CardIdentity, ToolRegistry};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::wave_report::WaveReportPayload;
use serde_json::{Value, json};

struct Boot {
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    repo: Arc<dyn Repo>,
    wave_id: WaveId,
    spec_card_id: CardId,
    worker_card_id: CardId,
    report_card_id: CardId,
    other_wave_card_id: CardId,
}

async fn boot() -> Boot {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "wave-file-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "wave file test".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let spec_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: Some(0.0),
            payload: json!({ "role": "spec" }),
        })
        .await
        .unwrap();
    let worker_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: Some(1.0),
            payload: json!({ "task": "local" }),
        })
        .await
        .unwrap();
    let report_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "wave-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(WaveReportPayload::initial()).unwrap(),
        })
        .await
        .unwrap();

    let cove2 = repo
        .cove_create(NewCove {
            name: "wave-file-other".into(),
            color: "#0f0".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave2 = repo
        .wave_create(NewWave {
            cove_id: cove2.id,
            title: "other wave".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let other_wave_card = repo
        .card_create(NewCard {
            wave_id: wave2.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: json!({ "task": "other-wave" }),
        })
        .await
        .unwrap();

    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    card_role_cache.insert(spec_card.id.clone(), CardRole::Spec, wave.id.clone());
    card_role_cache.insert(worker_card.id.clone(), CardRole::Worker, wave.id.clone());
    card_role_cache.insert(
        report_card.id.clone(),
        CardRole::ReportCard,
        wave.id.clone(),
    );
    card_role_cache.insert(
        other_wave_card.id.clone(),
        CardRole::Worker,
        wave2.id.clone(),
    );

    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let ctx = Arc::new(AppContext {
        repo: route_repo,
        events,
        card_role_cache,
        wave_cove_cache,
    });

    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);
    let registry = Arc::new(registry);

    Boot {
        ctx,
        registry,
        repo,
        wave_id: wave.id,
        spec_card_id: spec_card.id,
        worker_card_id: worker_card.id,
        report_card_id: report_card.id,
        other_wave_card_id: other_wave_card.id,
    }
}

async fn call_tool(
    boot: &Boot,
    name: &str,
    identity: CardIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let handler = boot
        .registry
        .lookup(name)
        .unwrap_or_else(|| panic!("tool not registered: {name}"));
    handler(boot.ctx.clone(), identity, args).await
}

fn spec_identity(boot: &Boot) -> CardIdentity {
    CardIdentity {
        card_id: boot.spec_card_id.clone(),
        role: CardRole::Spec,
    }
}

fn worker_identity(boot: &Boot) -> CardIdentity {
    CardIdentity {
        card_id: boot.worker_card_id.clone(),
        role: CardRole::Worker,
    }
}

fn content_json(value: &Value) -> Value {
    let content = value
        .get("content")
        .and_then(Value::as_str)
        .expect("content string");
    serde_json::from_str(content).expect("content is JSON")
}

#[tokio::test]
async fn ls_root_returns_top_level_entries() {
    let boot = boot().await;
    let out = call_tool(
        &boot,
        TOOL_WAVE_LS,
        spec_identity(&boot),
        json!({ "path": "/" }),
    )
    .await
    .expect("spec can list root");
    let entries = out.as_array().expect("ls returns array");
    let names: Vec<&str> = entries
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"index.md"), "entries = {entries:?}");
    assert!(names.contains(&"wave.json"), "entries = {entries:?}");
    assert!(names.contains(&"report.md"), "entries = {entries:?}");
    assert!(names.contains(&"cards/"), "entries = {entries:?}");
    let cards = entries
        .iter()
        .find(|entry| entry["name"] == "cards/")
        .expect("cards dir");
    assert_eq!(cards["kind"], json!("dir"));
}

#[tokio::test]
async fn cards_index_lists_only_bound_wave_cards_without_payload() {
    let boot = boot().await;
    let out = call_tool(
        &boot,
        TOOL_WAVE_CAT,
        spec_identity(&boot),
        json!({ "path": "cards/index.json" }),
    )
    .await
    .expect("spec can read card index");
    assert_eq!(out["content_type"], json!("application/json"));
    let cards = content_json(&out);
    let cards = cards.as_array().expect("card index is array");
    let ids: Vec<&str> = cards
        .iter()
        .map(|card| card["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&boot.spec_card_id.as_str()));
    assert!(ids.contains(&boot.worker_card_id.as_str()));
    assert!(ids.contains(&boot.report_card_id.as_str()));
    assert!(!ids.contains(&boot.other_wave_card_id.as_str()));
    assert!(
        cards.iter().all(|card| card.get("payload").is_none()),
        "cards/index.json must not include payloads: {cards:?}"
    );
}

#[tokio::test]
async fn card_payload_from_other_wave_is_forbidden() {
    let boot = boot().await;
    let path = format!("cards/{}/payload.json", boot.other_wave_card_id.as_str());
    let err = call_tool(
        &boot,
        TOOL_WAVE_CAT,
        spec_identity(&boot),
        json!({ "path": path }),
    )
    .await
    .expect_err("cross-wave card payload must be denied");
    assert_eq!(err.code, -32403);
    assert!(err.message.contains("forbidden"), "err = {err:?}");
}

#[tokio::test]
async fn wave_file_tools_refuse_worker() {
    let boot = boot().await;
    let err = call_tool(&boot, TOOL_WAVE_LS, worker_identity(&boot), json!({}))
        .await
        .expect_err("worker must be denied");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("Spec"), "err = {err:?}");
}

#[tokio::test]
async fn unknown_paths_return_clean_errors() {
    let boot = boot().await;
    let local_bad = format!("cards/{}/nonexistent", boot.worker_card_id.as_str());
    for path in ["runs/anything", "foo", local_bad.as_str()] {
        let err = call_tool(
            &boot,
            TOOL_WAVE_CAT,
            spec_identity(&boot),
            json!({ "path": path }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
        assert!(
            err.message.contains("path not available in this view"),
            "path {path} err = {err:?}"
        );
    }
}

#[tokio::test]
async fn index_md_uses_bound_wave_title_and_id() {
    let boot = boot().await;
    let out = call_tool(
        &boot,
        TOOL_WAVE_CAT,
        spec_identity(&boot),
        json!({ "path": "index.md" }),
    )
    .await
    .expect("index.md cat works");
    assert_eq!(out["content_type"], json!("text/markdown"));
    let content = out["content"].as_str().expect("markdown content");
    assert!(
        content.starts_with(&format!("# Wave {}", boot.wave_id.as_str())),
        "index.md should start with the bound wave id: {content:?}"
    );
    assert!(
        content.contains("- Title: wave file test"),
        "index.md should contain the bound wave title: {content:?}"
    );
}

#[tokio::test]
async fn report_md_matches_report_read_body() {
    let boot = boot().await;
    let report = call_tool(&boot, TOOL_REPORT_READ, spec_identity(&boot), json!({}))
        .await
        .expect("report read works");
    let report_body = report["body"].as_str().expect("report body");

    let file = call_tool(
        &boot,
        TOOL_WAVE_CAT,
        spec_identity(&boot),
        json!({ "path": "report.md" }),
    )
    .await
    .expect("report.md cat works");
    assert_eq!(file["content_type"], json!("text/markdown"));
    assert_eq!(file["content"].as_str(), Some(report_body));
}

#[tokio::test]
async fn wave_json_uses_bound_wave_metadata() {
    let boot = boot().await;
    let out = call_tool(
        &boot,
        TOOL_WAVE_CAT,
        spec_identity(&boot),
        json!({ "path": "wave.json" }),
    )
    .await
    .expect("wave.json cat works");
    let wave = content_json(&out);
    assert_eq!(wave["id"], json!(boot.wave_id.as_str()));

    let repo_wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wave["created_at"], json!(repo_wave.created_at));
}
