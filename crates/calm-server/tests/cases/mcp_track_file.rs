//! Issue #339 PR A — read-only track file MCP tools.
//!
//! Drives `calm.track.ls` / `calm.track.cat` through the default registry
//! against an in-memory repo. The tools derive scope from the
//! per-call `ToolCallIdentity`; none of the calls accepts a track id.

#![cfg(unix)]

use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::{ActorId, AreaId, CardId, TrackId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::track_file::{TOOL_TRACK_CAT, TOOL_TRACK_LS};
use calm_server::mcp_server::tools::track_history::{
    TOOL_TRACK_CAT_AT, TOOL_TRACK_DIFF, TOOL_TRACK_LOG,
};
use calm_server::mcp_server::tools::track_report::{TOOL_REPORT_READ, TOOL_REPORT_WRITE};
use calm_server::mcp_server::tools::track_state::TOOL_TASK_VERDICT;
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{CardRole, CardRuntimeView, NewArea, NewCard, NewTrack, now_ms};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionProjection,
    WorkerSessionState,
};
use calm_server::track_report::TrackReportPayload;
use calm_server::track_vcs;
use serde_json::{Value, json};

const PLANNER_SESSION_ID: &str = "planner-session";

struct Boot {
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    sqlx_repo: Arc<SqlxRepo>,
    repo: Arc<dyn Repo>,
    /// The CONFIGURED gate-logs dir wired into `AppContext` (PR #685
    /// F3) — `calm.track.cat plan/<key>/gate.log` must read THIS dir,
    /// never an env-recomputed default.
    gate_logs_dir: std::path::PathBuf,
    area_id: AreaId,
    track_id: TrackId,
    other_area_id: AreaId,
    other_track_id: TrackId,
    planner_card_id: CardId,
    worker_card_id: CardId,
    report_card_id: CardId,
    other_planner_card_id: CardId,
    other_track_card_id: CardId,
}

async fn seed_planner_root_runtime(repo: &SqlxRepo, planner_card_id: &CardId) {
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: PLANNER_SESSION_ID.to_string(),
            card_id: planner_card_id.as_str().to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some("planner-thread".into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .expect("seed planner root runtime");
    tx.commit().await.unwrap();
}

async fn boot() -> Boot {
    let sqlx_repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let area = repo
        .area_create(NewArea {
            name: "track-file-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "track file test".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let planner_card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: Some(0.0),
            payload: json!({ "role": "planner" }),
        })
        .await
        .unwrap();
    let worker_card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: Some(1.0),
            payload: json!({ "task": "local" }),
        })
        .await
        .unwrap();
    let report_card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "track-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
        })
        .await
        .unwrap();
    seed_planner_root_runtime(sqlx_repo.as_ref(), &planner_card.id).await;

    let area2 = repo
        .area_create(NewArea {
            name: "track-file-other".into(),
            color: "#0f0".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track2 = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area2.id.clone(),
            title: "other track".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let other_track_card = repo
        .card_create(NewCard {
            track_id: track2.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({ "task": "other-track" }),
        })
        .await
        .unwrap();
    let other_planner_card = repo
        .card_create(NewCard {
            track_id: track2.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: Some(-1.0),
            payload: json!({ "role": "planner" }),
        })
        .await
        .unwrap();

    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    card_role_cache.insert(planner_card.id.clone(), CardRole::Planner, track.id.clone());
    card_role_cache.insert(worker_card.id.clone(), CardRole::Worker, track.id.clone());
    card_role_cache.insert(
        report_card.id.clone(),
        CardRole::ReportCard,
        track.id.clone(),
    );
    card_role_cache.insert(
        other_track_card.id.clone(),
        CardRole::Worker,
        track2.id.clone(),
    );
    card_role_cache.insert(
        other_planner_card.id.clone(),
        CardRole::Planner,
        track2.id.clone(),
    );
    // #1189 §3.6 — the recorder gate reads `cards.role` in-tx, so the two
    // planner cards must be persisted as such and not merely cached that way
    // (`Repo::card_create` mints non-report cards as Worker).
    for card in [&planner_card.id, &other_planner_card.id] {
        crate::support::mcp::set_persisted_card_role(
            repo.as_ref(),
            card.as_str(),
            CardRole::Planner,
        )
        .await;
    }

    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();
    let gate_logs_dir = std::env::temp_dir().join(format!(
        "neige-track-file-gate-logs-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let ctx = Arc::new(AppContext {
        repo: route_repo,
        track_vcs: Some(calm_truth::track_vcs_repo::SqlxTrackVcsRepo::shared(
            sqlx_repo.pool().clone(),
        )),
        events,
        write: calm_server::state::WriteContext::new(card_role_cache, track_area_cache),
        daemon_token_hash: None,
        gate_logs_dir: gate_logs_dir.clone(),
        task_budget_default: calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
        plugin_host: Arc::new(tokio::sync::OnceCell::new()),
        operation_runtime: Arc::new(tokio::sync::OnceCell::new()),
    });

    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);
    let registry = Arc::new(registry);

    Boot {
        ctx,
        registry,
        sqlx_repo,
        repo,
        gate_logs_dir,
        area_id: area.id,
        track_id: track.id,
        other_area_id: area2.id,
        other_track_id: track2.id,
        planner_card_id: planner_card.id,
        worker_card_id: worker_card.id,
        report_card_id: report_card.id,
        other_planner_card_id: other_planner_card.id,
        other_track_card_id: other_track_card.id,
    }
}

async fn call_tool(
    boot: &Boot,
    name: &str,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let handler = boot
        .registry
        .lookup(name)
        .unwrap_or_else(|| panic!("tool not registered: {name}"));
    handler(boot.ctx.clone(), identity, args).await
}

fn planner_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.planner_card_id.as_str().to_string(),
        role: CardRole::Planner,
        provider: AgentProvider::Codex,
        session_id: PLANNER_SESSION_ID.to_string(),
        track_id: Some(boot.track_id.as_str().to_string()),
        area_id: boot.area_id.as_str().to_string(),
        thread_id: "planner-thread".to_string(),
    }
}

fn worker_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.worker_card_id.as_str().to_string(),
        role: CardRole::Worker,
        provider: AgentProvider::Codex,
        session_id: "worker-session".to_string(),
        track_id: Some(boot.track_id.as_str().to_string()),
        area_id: boot.area_id.as_str().to_string(),
        thread_id: "worker-thread".to_string(),
    }
}

fn other_planner_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.other_planner_card_id.as_str().to_string(),
        role: CardRole::Planner,
        provider: AgentProvider::Codex,
        session_id: "other-planner-session".to_string(),
        track_id: Some(boot.other_track_id.as_str().to_string()),
        area_id: boot.other_area_id.as_str().to_string(),
        thread_id: "other-planner-thread".to_string(),
    }
}

fn content_json(value: &Value) -> Value {
    let content = value
        .get("content")
        .and_then(Value::as_str)
        .expect("content string");
    serde_json::from_str(content).expect("content is JSON")
}

fn entry_updated_at(entries: &[Value], name: &str) -> i64 {
    entries
        .iter()
        .find(|entry| entry["name"] == name)
        .unwrap_or_else(|| panic!("missing {name}: {entries:?}"))["updated_at"]
        .as_i64()
        .expect("entry updated_at is i64")
}

#[allow(deprecated)]
async fn log_track_event(boot: &Boot, track_id: &TrackId, area_id: &AreaId, event: Event) -> i64 {
    log_track_event_as(boot, ActorId::User, track_id, area_id, event).await
}

#[allow(deprecated)]
async fn log_track_event_as(
    boot: &Boot,
    actor: ActorId,
    track_id: &TrackId,
    area_id: &AreaId,
    event: Event,
) -> i64 {
    boot.repo
        .log_pure_event(
            actor,
            EventScope::Track {
                track: track_id.clone(),
                area: area_id.clone(),
            },
            None,
            &boot.ctx.events,
            boot.ctx.write.role_cache(),
            boot.ctx.write.area_cache(),
            event,
        )
        .await
        .expect("log event")
}

#[allow(deprecated)]
async fn log_worker_card_event(boot: &Boot, event: Event) -> i64 {
    boot.repo
        .log_pure_event(
            ActorId::AiCodex(boot.worker_card_id.clone()),
            EventScope::Card {
                card: boot.worker_card_id.clone(),
                track: boot.track_id.clone(),
                area: boot.area_id.clone(),
            },
            None,
            &boot.ctx.events,
            boot.ctx.write.role_cache(),
            boot.ctx.write.area_cache(),
            event,
        )
        .await
        .expect("log worker card event")
}

#[allow(deprecated)]
async fn log_card_hook_event(boot: &Boot, card_id: &CardId, event: Event) -> i64 {
    let actor = match &event {
        Event::CodexHook { .. } => ActorId::AiCodex(card_id.clone()),
        Event::ClaudeHook { .. } => ActorId::AiClaude(card_id.clone()),
        _ => ActorId::User,
    };
    boot.repo
        .log_pure_event(
            actor,
            EventScope::Card {
                card: card_id.clone(),
                track: boot.track_id.clone(),
                area: boot.area_id.clone(),
            },
            None,
            &boot.ctx.events,
            boot.ctx.write.role_cache(),
            boot.ctx.write.area_cache(),
            event,
        )
        .await
        .expect("log card hook event")
}

async fn set_event_at(boot: &Boot, event_id: i64, at: i64) {
    sqlx::query("UPDATE events SET at = ?1 WHERE id = ?2")
        .bind(at)
        .bind(event_id)
        .execute(boot.sqlx_repo.pool())
        .await
        .expect("set event timestamp");
}

async fn set_card_updated_at(boot: &Boot, card_id: &CardId, updated_at: i64) {
    sqlx::query("UPDATE cards SET updated_at = ?1 WHERE id = ?2")
        .bind(updated_at)
        .bind(card_id.as_str())
        .execute(boot.sqlx_repo.pool())
        .await
        .expect("set card updated_at");
}

async fn request_codex(boot: &Boot, key: &str) -> i64 {
    log_track_event(
        boot,
        &boot.track_id,
        &boot.area_id,
        Event::CodexWorkerRequested {
            idempotency_key: key.into(),
            goal: format!("goal for {key}"),
            context: json!({ "key": key }),
            acceptance_criteria: Some(format!("accept {key}")),
            agent_message: None,
        },
    )
    .await
}

async fn complete_run(boot: &Boot, key: &str, summary: &str) -> i64 {
    complete_run_with_result(boot, key, json!({ "summary": summary })).await
}

async fn complete_run_with_result(boot: &Boot, key: &str, result: Value) -> i64 {
    log_worker_card_event(
        boot,
        Event::TaskCompleted {
            idempotency_key: key.into(),
            result,
            artifacts: vec![],
            agent_message: None,
        },
    )
    .await
}

async fn accept_run(boot: &Boot, key: &str, reason: &str) {
    call_tool(
        boot,
        TOOL_TASK_VERDICT,
        planner_identity(boot),
        json!({
            "idempotency_key": key,
            "status": "accepted",
            "reason": reason,
            "message": format!("accept {key}"),
        }),
    )
    .await
    .expect("planner can accept run");
}

async fn reject_run(boot: &Boot, key: &str, reason: &str) {
    call_tool(
        boot,
        TOOL_TASK_VERDICT,
        planner_identity(boot),
        json!({
            "idempotency_key": key,
            "status": "rejected",
            "reason": reason,
            "message": format!("reject {key}"),
        }),
    )
    .await
    .expect("planner can reject run");
}

async fn worker_fail_run(boot: &Boot, key: &str, reason: &str) -> i64 {
    log_worker_card_event(
        boot,
        Event::TaskFailed {
            idempotency_key: key.into(),
            reason: reason.into(),
            agent_message: None,
        },
    )
    .await
}

#[allow(deprecated)]
async fn materialize_worker(boot: &Boot, key: &str) -> CardId {
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: boot.track_id.clone(),
            title: None,
            kind: "codex".into(),
            sort: Some(10.0),
            payload: json!({
                "idempotency_key": key,
                "goal": format!("goal for {key}"),
                "context": { "key": key },
                "acceptance_criteria": format!("accept {key}"),
                "role_request": "codex",
                "prompt": format!("prompt for {key}")
            }),
        })
        .await
        .expect("create worker card");
    boot.ctx
        .write
        .role_cache()
        .insert(card.id.clone(), CardRole::Worker, boot.track_id.clone());
    card.id
}

async fn seed_codex_runtime(boot: &Boot, card_id: &CardId) -> WorkerSessionProjection {
    let runtime_id = calm_server::model::new_id();
    let mut tx = boot.sqlx_repo.pool().begin().await.unwrap();
    let runtime = session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card_id.as_str().to_string(),
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Running,
            terminal_run_id: None,
            thread_id: Some("thread-runtime-json".into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .expect("seed runtime row");
    tx.commit().await.unwrap();
    runtime
}

#[tokio::test]
async fn ls_root_returns_top_level_entries() {
    let boot = boot().await;
    let out = call_tool(
        &boot,
        TOOL_TRACK_LS,
        planner_identity(&boot),
        json!({ "path": "/" }),
    )
    .await
    .expect("planner can list root");
    let entries = out.as_array().expect("ls returns array");
    let names: Vec<&str> = entries
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"index.md"), "entries = {entries:?}");
    assert!(names.contains(&"track.json"), "entries = {entries:?}");
    assert!(names.contains(&"report.md"), "entries = {entries:?}");
    assert!(names.contains(&"cards/"), "entries = {entries:?}");
    assert!(names.contains(&"runs/"), "entries = {entries:?}");
    let cards = entries
        .iter()
        .find(|entry| entry["name"] == "cards/")
        .expect("cards dir");
    assert_eq!(cards["kind"], json!("dir"));
    let runs = entries
        .iter()
        .find(|entry| entry["name"] == "runs/")
        .expect("runs dir");
    assert_eq!(runs["kind"], json!("dir"));
}

#[tokio::test]
async fn cards_index_lists_only_bound_track_cards_without_payload() {
    let boot = boot().await;
    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "cards/index.json" }),
    )
    .await
    .expect("planner can read card index");
    assert_eq!(out["content_type"], json!("application/json"));
    let cards = content_json(&out);
    let cards = cards.as_array().expect("card index is array");
    let ids: Vec<&str> = cards
        .iter()
        .map(|card| card["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&boot.planner_card_id.as_str()));
    assert!(ids.contains(&boot.worker_card_id.as_str()));
    assert!(ids.contains(&boot.report_card_id.as_str()));
    assert!(!ids.contains(&boot.other_track_card_id.as_str()));
    assert!(
        cards.iter().all(|card| card.get("payload").is_none()),
        "cards/index.json must not include payloads: {cards:?}"
    );
}

#[tokio::test]
async fn card_events_json_returns_hook_events_in_event_order() {
    let boot = boot().await;
    let card_id = boot.worker_card_id.clone();
    let first_payload = json!({
        "hook_event_name": "UserPromptSubmit",
        "prompt": "build the reader view",
        "transcript_path": "/home/kenji/.claude/projects/private/session.jsonl"
    });
    let second_payload = json!({
        "hook_event_name": "Stop",
        "last_assistant_message": "implemented",
        "nested": { "kept": true }
    });
    let first_id = log_card_hook_event(
        &boot,
        &card_id,
        Event::ClaudeHook {
            card_id: card_id.clone(),
            kind: "hook.claude.user_prompt_submit".into(),
            hook_idempotency_key: "hook-key".into(),
            payload: first_payload.clone(),
        },
    )
    .await;
    let second_id = log_card_hook_event(
        &boot,
        &card_id,
        Event::CodexHook {
            card_id: card_id.clone(),
            kind: "hook.codex.stop".into(),
            hook_idempotency_key: "hook-key".into(),
            payload: second_payload.clone(),
        },
    )
    .await;
    set_event_at(&boot, first_id, 200).await;
    set_event_at(&boot, second_id, 100).await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": format!("cards/{}/events.json", card_id.as_str()) }),
    )
    .await
    .expect("planner can read card hook events");
    assert_eq!(out["content_type"], json!("application/json"));
    assert_eq!(
        content_json(&out),
        json!([
            {
                "event_id": first_id,
                "kind": "claude.hook",
                "hook_kind": "hook.claude.user_prompt_submit",
                "created_at": 200,
                "payload": first_payload,
            },
            {
                "event_id": second_id,
                "kind": "codex.hook",
                "hook_kind": "hook.codex.stop",
                "created_at": 100,
                "payload": second_payload,
            }
        ])
    );
}

#[tokio::test]
async fn card_conversation_md_renders_prompt_tool_and_assistant_turns() {
    let boot = boot().await;
    let card_id = boot.worker_card_id.clone();
    log_card_hook_event(
        &boot,
        &card_id,
        Event::ClaudeHook {
            card_id: card_id.clone(),
            kind: "hook.claude.user_prompt_submit".into(),
            hook_idempotency_key: "hook-key".into(),
            payload: json!({
                "hook_event_name": "UserPromptSubmit",
                "prompt": "Please inspect the hook history."
            }),
        },
    )
    .await;
    log_card_hook_event(
        &boot,
        &card_id,
        Event::ClaudeHook {
            card_id: card_id.clone(),
            kind: "hook.claude.pre_tool_use".into(),
            hook_idempotency_key: "hook-key".into(),
            payload: json!({
                "hook_event_name": "PreToolUse",
                "tool_name": "Read"
            }),
        },
    )
    .await;
    log_card_hook_event(
        &boot,
        &card_id,
        Event::CodexHook {
            card_id: card_id.clone(),
            kind: "hook.codex.stop".into(),
            hook_idempotency_key: "hook-key".into(),
            payload: json!({
                "last_assistant_message": "The projection is ready."
            }),
        },
    )
    .await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": format!("cards/{}/conversation.md", card_id.as_str()) }),
    )
    .await
    .expect("planner can read conversation projection");
    assert_eq!(out["content_type"], json!("text/markdown"));
    let md = out["content"].as_str().expect("markdown content");
    assert!(md.starts_with(
        "> READ-ONLY PROJECTION: derived from persisted track hook events. This is not the source of truth."
    ));
    assert!(md.contains(&format!("# Conversation — card {}", card_id.as_str())));
    assert!(md.contains("## User\n\nPlease inspect the hook history."));
    assert!(md.contains("- tool: Read"));
    assert!(md.contains("## Assistant\n\nThe projection is ready."));
    let user = md.find("## User").expect("user section");
    let tool = md.find("- tool: Read").expect("tool summary");
    let assistant = md.find("## Assistant").expect("assistant section");
    assert!(user < tool && tool < assistant, "md = {md}");
}

#[tokio::test]
async fn card_conversation_md_ignores_subagent_stop_assistant_message() {
    let boot = boot().await;
    let card_id = boot.worker_card_id.clone();
    log_card_hook_event(
        &boot,
        &card_id,
        Event::ClaudeHook {
            card_id: card_id.clone(),
            kind: "hook.claude.subagent_stop".into(),
            hook_idempotency_key: "hook-key".into(),
            payload: json!({
                "hook_event_name": "SubagentStop",
                "last_assistant_message": "subagent completion must not leak"
            }),
        },
    )
    .await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": format!("cards/{}/conversation.md", card_id.as_str()) }),
    )
    .await
    .expect("planner can read conversation projection");
    assert_eq!(out["content_type"], json!("text/markdown"));
    let md = out["content"].as_str().expect("markdown content");
    assert!(!md.contains("## Assistant"), "md = {md}");
    assert!(
        !md.contains("subagent completion must not leak"),
        "md = {md}"
    );
}

#[tokio::test]
async fn card_conversation_md_reports_no_hook_events() {
    let boot = boot().await;
    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": format!("cards/{}/conversation.md", boot.worker_card_id.as_str()) }),
    )
    .await
    .expect("planner can read empty conversation projection");
    assert_eq!(out["content_type"], json!("text/markdown"));
    let md = out["content"].as_str().expect("markdown content");
    assert!(md.contains("_No hook events recorded._"), "md = {md}");
}

#[tokio::test]
async fn card_conversation_md_renders_worker_flow_when_present() {
    // #695 PR3: when the worker-flow capture table has rows for a card,
    // conversation.md projects the captured transcript instead of the hook
    // events.
    let boot = boot().await;
    let card_id = boot.worker_card_id.clone();
    let runtime = seed_codex_runtime(&boot, &card_id).await;

    let user = json!({
        "type": "userMessage",
        "seq": 0, "turn": 1,
        "session_id": "sess-1", "provider": "codex",
        "timestamp": null, "source_uuid": null,
        "provider_extra": null, "raw_ref": null,
        "content": [{ "type": "text", "text": "Run the build" }]
    });
    let cmd = json!({
        "type": "commandExecution",
        "seq": 1, "turn": 1,
        "session_id": "sess-1", "provider": "codex",
        "timestamp": null, "source_uuid": null,
        "provider_extra": null, "raw_ref": null,
        "call_id": "c1", "command": "cargo build", "cwd": null,
        "parsed_actions": [], "aggregated_output": null,
        "exit_code": 0, "duration_ms": null,
        "status": "completed", "source": "agent"
    });
    let answer = json!({
        "type": "agentMessage",
        "seq": 2, "turn": 1,
        "session_id": "sess-1", "provider": "codex",
        "timestamp": null, "source_uuid": null,
        "provider_extra": null, "raw_ref": null,
        "text": "Build is green.", "is_final": true, "phase": null
    });
    for (kind, payload) in [
        ("userMessage", &user),
        ("commandExecution", &cmd),
        ("agentMessage", &answer),
    ] {
        boot.repo
            .worker_flow_item_insert(
                Some(card_id.as_str()),
                Some(runtime.id.as_str()),
                Some(boot.track_id.as_str()),
                Some(runtime.id.as_str()),
                kind,
                &serde_json::to_string(payload).unwrap(),
                now_ms(),
            )
            .await
            .expect("insert worker flow item");
    }

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": format!("cards/{}/conversation.md", card_id.as_str()) }),
    )
    .await
    .expect("planner can read worker-flow projection");
    assert_eq!(out["content_type"], json!("text/markdown"));
    let md = out["content"].as_str().expect("markdown content");
    assert!(md.starts_with(
        "> READ-ONLY PROJECTION: derived from persisted worker flow items. This is not the source of truth."
    ));
    assert!(md.contains("## User\n\nRun the build"), "md = {md}");
    assert!(md.contains("- ran `cargo build` ✓"), "md = {md}");
    assert!(md.contains("## Assistant\n\nBuild is green."), "md = {md}");
}

#[tokio::test]
async fn card_conversation_md_falls_back_to_hooks_when_no_flow_rows() {
    // #695 PR3 no-regression: with NO worker_flow_items rows, conversation.md
    // still renders the existing hook-event projection.
    let boot = boot().await;
    let card_id = boot.worker_card_id.clone();
    log_card_hook_event(
        &boot,
        &card_id,
        Event::ClaudeHook {
            card_id: card_id.clone(),
            kind: "hook.claude.user_prompt_submit".into(),
            hook_idempotency_key: "hook-key".into(),
            payload: json!({
                "hook_event_name": "UserPromptSubmit",
                "prompt": "From the hook path."
            }),
        },
    )
    .await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": format!("cards/{}/conversation.md", card_id.as_str()) }),
    )
    .await
    .expect("planner can read hook projection fallback");
    let md = out["content"].as_str().expect("markdown content");
    assert!(md.contains("## User\n\nFrom the hook path."), "md = {md}");
    // Worker-flow-only sentinel must NOT appear on the hook path.
    assert!(
        !md.contains("_No worker-flow items recorded._"),
        "md = {md}"
    );
}

#[tokio::test]
async fn ls_card_directory_includes_hook_event_views() {
    let boot = boot().await;
    let card_id = boot.worker_card_id.clone();
    set_card_updated_at(&boot, &card_id, 100).await;
    let event_id = log_card_hook_event(
        &boot,
        &card_id,
        Event::ClaudeHook {
            card_id: card_id.clone(),
            kind: "hook.claude.stop".into(),
            hook_idempotency_key: "hook-key".into(),
            payload: json!({
                "hook_event_name": "Stop",
                "last_assistant_message": "done"
            }),
        },
    )
    .await;
    set_event_at(&boot, event_id, 900).await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_LS,
        planner_identity(&boot),
        json!({ "path": format!("cards/{}", card_id.as_str()) }),
    )
    .await
    .expect("planner can list card directory");
    let entries = out.as_array().expect("ls returns array");
    let names: Vec<&str> = entries
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&".meta.json"), "entries = {entries:?}");
    assert!(names.contains(&".payload.json"), "entries = {entries:?}");
    assert!(names.contains(&"runtime.json"), "entries = {entries:?}");
    assert!(names.contains(&"events.json"), "entries = {entries:?}");
    assert!(names.contains(&"conversation.md"), "entries = {entries:?}");
    for leaf in [".meta.json", ".payload.json", "runtime.json"] {
        let entry = entries
            .iter()
            .find(|entry| entry["name"] == leaf)
            .unwrap_or_else(|| panic!("missing {leaf}: {entries:?}"));
        assert_eq!(entry["updated_at"], json!(100));
    }
    for leaf in ["events.json", "conversation.md"] {
        let entry = entries
            .iter()
            .find(|entry| entry["name"] == leaf)
            .unwrap_or_else(|| panic!("missing {leaf}: {entries:?}"));
        assert_eq!(entry["updated_at"], json!(900));
    }
}

#[tokio::test]
async fn old_card_lens_paths_are_not_available() {
    let boot = boot().await;

    for leaf in ["meta.json", "payload.json"] {
        let path = format!("cards/{}/{leaf}", boot.worker_card_id.as_str());
        let err = call_tool(
            &boot,
            TOOL_TRACK_CAT,
            planner_identity(&boot),
            json!({ "path": path }),
        )
        .await
        .expect_err("legacy card lens path must be unavailable");
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
        assert!(
            err.message.contains("path not available in this view"),
            "err = {err:?}"
        );
    }
}

#[tokio::test]
async fn card_runtime_json_returns_typed_runtime_or_null() {
    let boot = boot().await;
    let card_id = boot.worker_card_id.clone();
    set_card_updated_at(&boot, &card_id, 100).await;

    let listing = call_tool(
        &boot,
        TOOL_TRACK_LS,
        planner_identity(&boot),
        json!({ "path": format!("cards/{}", card_id.as_str()) }),
    )
    .await
    .expect("planner can list card directory before runtime exists");
    let entries = listing.as_array().expect("ls returns array");
    assert_eq!(entry_updated_at(entries, "runtime.json"), 100);

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": format!("cards/{}/runtime.json", card_id.as_str()) }),
    )
    .await
    .expect("planner can read runtime projection");
    assert_eq!(out["content_type"], json!("application/json"));
    let runtime: Option<CardRuntimeView> =
        serde_json::from_value(content_json(&out)).expect("runtime projection is typed");
    assert!(runtime.is_none(), "cards without runtime rows return null");

    let session_projection_row = seed_codex_runtime(&boot, &card_id).await;
    let listing = call_tool(
        &boot,
        TOOL_TRACK_LS,
        planner_identity(&boot),
        json!({ "path": format!("cards/{}", card_id.as_str()) }),
    )
    .await
    .expect("planner can list card directory after runtime exists");
    let entries = listing.as_array().expect("ls returns array");
    let listed_updated_at = entry_updated_at(entries, "runtime.json");
    assert!(
        listed_updated_at >= session_projection_row.updated_at_ms,
        "runtime.json listing timestamp {listed_updated_at} should reflect runtime row {}",
        session_projection_row.updated_at_ms
    );

    let runtime_id = session_projection_row.id.clone();
    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": format!("cards/{}/runtime.json", card_id.as_str()) }),
    )
    .await
    .expect("planner can read typed runtime projection");
    let runtime: Option<CardRuntimeView> =
        serde_json::from_value(content_json(&out)).expect("runtime projection is typed");
    let runtime = runtime.expect("runtime row is projected");
    assert_eq!(runtime.runtime_id, runtime_id);
    assert_eq!(runtime.kind, WorkerSessionKind::CodexCard);
    assert_eq!(runtime.status, WorkerSessionState::Running);
    assert_eq!(runtime.provider, Some(AgentProvider::Codex));
    assert_eq!(runtime.thread_id.as_deref(), Some("thread-runtime-json"));
}

#[tokio::test]
async fn card_hook_events_from_other_track_are_forbidden() {
    let boot = boot().await;
    let path = format!("cards/{}/events.json", boot.worker_card_id.as_str());
    let err = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        other_planner_identity(&boot),
        json!({ "path": path }),
    )
    .await
    .expect_err("other track must not read bound-track card hook events");
    assert_eq!(err.code, -32403);
    assert!(err.message.contains("forbidden"), "err = {err:?}");
}

#[tokio::test]
async fn card_events_json_filters_out_sibling_card_hooks() {
    let boot = boot().await;
    let target_id = boot.worker_card_id.clone();
    let sibling_id = materialize_worker(&boot, "sibling-hooks").await;
    let target_event_id = log_card_hook_event(
        &boot,
        &target_id,
        Event::ClaudeHook {
            card_id: target_id.clone(),
            kind: "hook.claude.stop".into(),
            hook_idempotency_key: "hook-key".into(),
            payload: json!({
                "hook_event_name": "Stop",
                "last_assistant_message": "target only"
            }),
        },
    )
    .await;
    log_card_hook_event(
        &boot,
        &sibling_id,
        Event::ClaudeHook {
            card_id: sibling_id.clone(),
            kind: "hook.claude.stop".into(),
            hook_idempotency_key: "hook-key".into(),
            payload: json!({
                "hook_event_name": "Stop",
                "last_assistant_message": "sibling must not leak"
            }),
        },
    )
    .await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": format!("cards/{}/events.json", target_id.as_str()) }),
    )
    .await
    .expect("planner can read target events");
    let events = content_json(&out);
    let events = events.as_array().expect("events array");
    assert_eq!(events.len(), 1, "events = {events:?}");
    assert_eq!(events[0]["event_id"], json!(target_event_id));
    assert_eq!(
        events[0]["payload"]["last_assistant_message"],
        json!("target only")
    );
}

#[tokio::test]
async fn ls_runs_returns_projected_runs_for_bound_track() {
    let boot = boot().await;
    request_codex(&boot, "run-list").await;
    let worker_id = materialize_worker(&boot, "run-list").await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_LS,
        planner_identity(&boot),
        json!({ "path": "runs/" }),
    )
    .await
    .expect("planner can list runs");
    let runs = out.as_array().expect("runs ls returns array");
    let names = runs
        .iter()
        .map(|run| run["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["index.json", "run-list.md", "run-list.json"]);
    let entry = runs
        .iter()
        .find(|run| run["name"] == "run-list.md")
        .unwrap_or_else(|| panic!("missing run-list.md: {runs:?}"));
    assert_eq!(entry["kind"], json!("file"));
    assert_eq!(entry["idempotency_key"], json!("run-list"));
    assert_eq!(entry["status"], json!("running"));
    assert_eq!(entry["run_kind"], json!("codex"));
    assert_eq!(entry["worker_card_id"], json!(worker_id.as_str()));
}

#[tokio::test]
#[allow(deprecated)]
async fn runs_projection_ignores_non_worker_cards_with_idempotency_key_payloads() {
    let boot = boot().await;
    let decoy = boot
        .repo
        .card_create(NewCard {
            track_id: boot.track_id.clone(),
            title: None,
            kind: "planner".into(),
            sort: Some(2.0),
            payload: json!({ "idempotency_key": "decoy" }),
        })
        .await
        .expect("create decoy card");
    boot.ctx
        .write
        .role_cache()
        .insert(decoy.id, CardRole::Planner, boot.track_id.clone());

    request_codex(&boot, "real-run").await;
    materialize_worker(&boot, "real-run").await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/index.json" }),
    )
    .await
    .expect("planner can read runs index");
    let runs = content_json(&out);
    let runs = runs.as_array().expect("runs index is array");
    let keys: Vec<&str> = runs
        .iter()
        .map(|run| run["idempotency_key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["real-run"], "runs = {runs:?}");
}

#[tokio::test]
async fn runs_index_json_returns_same_run_set_as_ls_with_full_fields() {
    let boot = boot().await;
    request_codex(&boot, "run-a").await;
    request_codex(&boot, "run-b").await;
    materialize_worker(&boot, "run-b").await;

    let ls = call_tool(
        &boot,
        TOOL_TRACK_LS,
        planner_identity(&boot),
        json!({ "path": "runs" }),
    )
    .await
    .expect("planner can list runs");
    let ls_keys: Vec<&str> = ls
        .as_array()
        .unwrap()
        .iter()
        .filter(|run| {
            run["name"]
                .as_str()
                .is_some_and(|name| name.ends_with(".md"))
        })
        .map(|run| run["idempotency_key"].as_str().unwrap())
        .collect();

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/index.json" }),
    )
    .await
    .expect("planner can read runs index");
    assert_eq!(out["content_type"], json!("application/json"));
    let runs = content_json(&out);
    let runs = runs.as_array().expect("runs index is array");
    let index_keys: Vec<&str> = runs
        .iter()
        .map(|run| run["idempotency_key"].as_str().unwrap())
        .collect();
    assert_eq!(index_keys, ls_keys);
    for run in runs {
        assert!(run.get("status").is_some(), "run = {run:?}");
        assert!(run.get("kind").is_some(), "run = {run:?}");
        assert!(run.get("requested_at").is_some(), "run = {run:?}");
        assert!(run.get("finished_at").is_some(), "run = {run:?}");
        assert!(run.get("worker_card_id").is_some(), "run = {run:?}");
    }
}

#[tokio::test]
async fn completed_run_markdown_includes_read_only_banner_and_worker_fields() {
    let boot = boot().await;
    request_codex(&boot, "done-md").await;
    materialize_worker(&boot, "done-md").await;
    complete_run(&boot, "done-md", "finished cleanly").await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/done-md.md" }),
    )
    .await
    .expect("planner can read run markdown");
    assert_eq!(out["content_type"], json!("text/markdown"));
    let md = out["content"].as_str().expect("markdown content");
    assert!(md.contains("READ-ONLY PROJECTION"), "md = {md}");
    assert!(md.contains("- Status: completed"), "md = {md}");
    assert!(md.contains("## Goal"), "md = {md}");
    assert!(md.contains("goal for done-md"), "md = {md}");
    assert!(md.contains("## Context"), "md = {md}");
    assert!(md.contains("## Acceptance Criteria"), "md = {md}");
    assert!(md.contains("accept done-md"), "md = {md}");
    assert!(md.contains("## Prompt"), "md = {md}");
    assert!(md.contains("prompt for done-md"), "md = {md}");
    assert!(md.contains("TaskCompleted"), "md = {md}");
    assert!(md.contains("finished cleanly"), "md = {md}");
}

#[tokio::test]
async fn completed_run_json_returns_structured_projection() {
    let boot = boot().await;
    request_codex(&boot, "done-json").await;
    let worker_id = materialize_worker(&boot, "done-json").await;
    seed_codex_runtime(&boot, &worker_id).await;
    complete_run(&boot, "done-json", "json complete").await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/done-json.json" }),
    )
    .await
    .expect("planner can read run json");
    assert_eq!(out["content_type"], json!("application/json"));
    let run = content_json(&out);
    assert_eq!(run["idempotency_key"], json!("done-json"));
    assert_eq!(run["status"], json!("completed"));
    assert_eq!(run["kind"], json!("codex"));
    assert_eq!(run["worker_card_id"], json!(worker_id.as_str()));
    assert_eq!(
        run["worker_card_payload"]["prompt"],
        json!("prompt for done-json")
    );
    assert!(
        run["worker_card_payload"].get("codex_thread_id").is_none(),
        "worker_card_payload must stay raw: {run:?}"
    );
    assert!(
        run["worker_card_payload"]
            .get("codex_thread_status")
            .is_none(),
        "worker_card_payload must stay raw: {run:?}"
    );
    assert_eq!(
        run["events"]["requested"]["payload"]["idempotency_key"],
        json!("done-json")
    );
    assert_eq!(
        run["events"]["completed"]["payload"]["result"]["summary"],
        json!("json complete")
    );
    assert!(run["verdict"].is_null(), "run = {run:?}");
    assert!(run["events"]["verdict"].is_null(), "run = {run:?}");
    assert!(run["events"]["failed"].is_null(), "run = {run:?}");
}

#[tokio::test]
async fn worker_completion_result_status_accepted_is_not_planner_verdict() {
    let boot = boot().await;
    request_codex(&boot, "worker-accepted-payload").await;
    materialize_worker(&boot, "worker-accepted-payload").await;
    complete_run_with_result(
        &boot,
        "worker-accepted-payload",
        json!({ "status": "accepted", "summary": "yay" }),
    )
    .await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/worker-accepted-payload.json" }),
    )
    .await
    .expect("planner can read run json");
    let run = content_json(&out);
    assert_eq!(run["status"], json!("completed"));
    assert_eq!(
        run["events"]["completed"]["payload"]["result"],
        json!({ "status": "accepted", "summary": "yay" })
    );
    assert!(run["verdict"].is_null(), "run = {run:?}");
    assert!(run["events"]["verdict"].is_null(), "run = {run:?}");
}

#[tokio::test]
async fn accepted_verdict_does_not_overwrite_worker_completion() {
    let boot = boot().await;
    request_codex(&boot, "accepted-run").await;
    materialize_worker(&boot, "accepted-run").await;
    complete_run(&boot, "accepted-run", "did the thing").await;
    accept_run(&boot, "accepted-run", "LGTM").await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/accepted-run.json" }),
    )
    .await
    .expect("planner can read accepted run json");
    let run = content_json(&out);
    assert_eq!(run["status"], json!("completed"));
    assert_eq!(
        run["events"]["completed"]["payload"]["result"],
        json!({ "summary": "did the thing" })
    );
    assert_eq!(run["verdict"]["status"], json!("accepted"));
    assert_eq!(run["verdict"]["reason"], json!("LGTM"));
    assert_eq!(
        run["events"]["verdict"]["payload"]["result"],
        json!({ "status": "accepted", "reason": "LGTM" })
    );

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/index.json" }),
    )
    .await
    .expect("planner can read runs index");
    let runs = content_json(&out);
    let entry = runs
        .as_array()
        .unwrap()
        .iter()
        .find(|run| run["idempotency_key"] == "accepted-run")
        .unwrap_or_else(|| panic!("missing accepted-run: {runs:?}"));
    assert_eq!(entry["status"], json!("completed"));
    assert_eq!(entry["verdict"]["status"], json!("accepted"));
    assert!(
        entry["verdict"].get("reason").is_none(),
        "entry = {entry:?}"
    );

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/accepted-run.md" }),
    )
    .await
    .expect("planner can read accepted run markdown");
    let md = out["content"].as_str().expect("markdown content");
    assert!(md.contains("## Verdict"), "md = {md}");
    assert!(
        md.contains("accepted by planner at") && md.contains(": LGTM"),
        "md = {md}"
    );
    assert!(md.contains("did the thing"), "md = {md}");
}

#[tokio::test]
async fn run_listing_updated_at_uses_latest_verdict_timestamp() {
    let boot = boot().await;
    let requested_id = request_codex(&boot, "verdict-mtime").await;
    let worker_id = materialize_worker(&boot, "verdict-mtime").await;
    let completed_id = complete_run(&boot, "verdict-mtime", "done before verdict").await;
    let verdict_id = log_track_event(
        &boot,
        &boot.track_id,
        &boot.area_id,
        Event::TaskCompleted {
            idempotency_key: "verdict-mtime".into(),
            result: json!({ "status": "accepted", "reason": "checked later" }),
            artifacts: vec![],
            agent_message: None,
        },
    )
    .await;
    set_event_at(&boot, requested_id, 50).await;
    set_event_at(&boot, completed_id, 100).await;
    set_card_updated_at(&boot, &worker_id, 150).await;
    set_event_at(&boot, verdict_id, 200).await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_LS,
        planner_identity(&boot),
        json!({ "path": "runs/" }),
    )
    .await
    .expect("planner can list runs");
    let runs = out.as_array().expect("runs ls returns array");
    let entry = runs
        .iter()
        .find(|run| run["idempotency_key"] == "verdict-mtime")
        .unwrap_or_else(|| panic!("missing verdict-mtime: {runs:?}"));
    assert_eq!(entry["finished_at"], json!(100));
    assert_eq!(entry["verdict"]["at"], json!(200));
    assert_eq!(entry["updated_at"], json!(200));
}

#[tokio::test]
async fn rejected_verdict_does_not_overwrite_worker_completion() {
    let boot = boot().await;
    request_codex(&boot, "rejected-run").await;
    materialize_worker(&boot, "rejected-run").await;
    complete_run(&boot, "rejected-run", "did stuff").await;
    reject_run(&boot, "rejected-run", "not enough detail").await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/rejected-run.json" }),
    )
    .await
    .expect("planner can read rejected run json");
    let run = content_json(&out);
    assert_eq!(run["status"], json!("completed"));
    assert_eq!(
        run["events"]["completed"]["payload"]["result"],
        json!({ "summary": "did stuff" })
    );
    assert!(run["events"]["failed"].is_null(), "run = {run:?}");
    assert_eq!(run["verdict"]["status"], json!("rejected"));
    assert_eq!(run["verdict"]["reason"], json!("not enough detail"));
    assert_eq!(
        run["events"]["verdict"]["payload"]["reason"],
        json!("not enough detail")
    );

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/rejected-run.md" }),
    )
    .await
    .expect("planner can read rejected run markdown");
    let md = out["content"].as_str().expect("markdown content");
    assert!(
        md.contains("Verdict: rejected by planner at") && md.contains(": not enough detail"),
        "md = {md}"
    );
}

#[tokio::test]
async fn worker_failure_without_planner_verdict_stays_failed() {
    let boot = boot().await;
    request_codex(&boot, "worker-failed").await;
    materialize_worker(&boot, "worker-failed").await;
    worker_fail_run(&boot, "worker-failed", "stub failure").await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/worker-failed.json" }),
    )
    .await
    .expect("planner can read worker-failed run json");
    let run = content_json(&out);
    assert_eq!(run["status"], json!("failed"));
    assert_eq!(
        run["events"]["failed"]["payload"]["reason"],
        json!("stub failure")
    );
    assert!(run["verdict"].is_null(), "run = {run:?}");
    assert!(run["events"]["verdict"].is_null(), "run = {run:?}");
}

#[tokio::test]
async fn track_scoped_dispatcher_failure_is_not_planner_verdict() {
    let boot = boot().await;
    log_track_event_as(
        &boot,
        ActorId::KernelDispatcher,
        &boot.track_id,
        &boot.area_id,
        Event::CodexWorkerRequested {
            idempotency_key: "dispatcher-track-failed".into(),
            goal: "track-scoped dispatcher request".into(),
            context: json!({}),
            acceptance_criteria: None,
            agent_message: None,
        },
    )
    .await;
    log_track_event_as(
        &boot,
        ActorId::KernelDispatcher,
        &boot.track_id,
        &boot.area_id,
        Event::TaskFailed {
            idempotency_key: "dispatcher-track-failed".into(),
            reason: "spawn failed".into(),
            agent_message: None,
        },
    )
    .await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/dispatcher-track-failed.json" }),
    )
    .await
    .expect("planner can read dispatcher-track-failed run json");
    let run = content_json(&out);
    assert_eq!(run["status"], json!("failed"));
    assert_eq!(
        run["events"]["failed"]["payload"]["reason"],
        json!("spawn failed")
    );
    assert!(run["verdict"].is_null(), "run = {run:?}");
    assert!(run["events"]["verdict"].is_null(), "run = {run:?}");
}

#[tokio::test]
async fn retry_recovery_uses_later_worker_completion_as_status() {
    let boot = boot().await;
    request_codex(&boot, "retry-recovered").await;
    materialize_worker(&boot, "retry-recovered").await;
    let failed_id = worker_fail_run(&boot, "retry-recovered", "spawn failed").await;
    let completed_id = complete_run_with_result(
        &boot,
        "retry-recovered",
        json!({ "summary": "second attempt worked" }),
    )
    .await;
    set_event_at(&boot, failed_id, 100).await;
    set_event_at(&boot, completed_id, 200).await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/retry-recovered.json" }),
    )
    .await
    .expect("planner can read retry-recovered run json");
    let run = content_json(&out);
    assert_eq!(run["status"], json!("completed"));
    assert_eq!(
        run["events"]["failed"]["payload"]["reason"],
        json!("spawn failed")
    );
    assert_eq!(
        run["events"]["completed"]["payload"]["result"],
        json!({ "summary": "second attempt worked" })
    );
}

#[tokio::test]
async fn dispatcher_retry_completion_overrides_earlier_completion() {
    let boot = boot().await;
    request_codex(&boot, "dispatcher-retry-recovered").await;
    materialize_worker(&boot, "dispatcher-retry-recovered").await;
    let first_completed_id = complete_run_with_result(
        &boot,
        "dispatcher-retry-recovered",
        json!({ "summary": "first attempt" }),
    )
    .await;
    let failed_id = worker_fail_run(
        &boot,
        "dispatcher-retry-recovered",
        "spawn failed mid-stream",
    )
    .await;
    let retry_completed_id = complete_run_with_result(
        &boot,
        "dispatcher-retry-recovered",
        json!({ "summary": "retry worked" }),
    )
    .await;
    set_event_at(&boot, first_completed_id, 100).await;
    set_event_at(&boot, failed_id, 200).await;
    set_event_at(&boot, retry_completed_id, 300).await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/dispatcher-retry-recovered.json" }),
    )
    .await
    .expect("planner can read dispatcher-retry-recovered run json");
    let run = content_json(&out);
    assert_eq!(run["status"], json!("completed"));
    assert_eq!(
        run["events"]["completed"]["payload"]["result"],
        json!({ "summary": "retry worked" })
    );
}

#[tokio::test]
async fn retry_regression_uses_later_worker_failure_as_status() {
    let boot = boot().await;
    request_codex(&boot, "retry-regressed").await;
    materialize_worker(&boot, "retry-regressed").await;
    let completed_id = complete_run(&boot, "retry-regressed", "first attempt worked").await;
    let failed_id = worker_fail_run(&boot, "retry-regressed", "retry failed").await;
    set_event_at(&boot, completed_id, 100).await;
    set_event_at(&boot, failed_id, 200).await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/retry-regressed.json" }),
    )
    .await
    .expect("planner can read retry-regressed run json");
    let run = content_json(&out);
    assert_eq!(run["status"], json!("failed"));
    assert_eq!(
        run["events"]["completed"]["payload"]["result"],
        json!({ "summary": "first attempt worked" })
    );
    assert_eq!(
        run["events"]["failed"]["payload"]["reason"],
        json!("retry failed")
    );
}

#[tokio::test]
async fn final_event_order_uses_event_id_not_wall_clock_at() {
    let boot = boot().await;
    request_codex(&boot, "retry-regressed-skew").await;
    materialize_worker(&boot, "retry-regressed-skew").await;
    let completed_id = complete_run(&boot, "retry-regressed-skew", "first attempt worked").await;
    let failed_id = worker_fail_run(&boot, "retry-regressed-skew", "retry failed").await;
    assert!(failed_id > completed_id);
    set_event_at(&boot, completed_id, 1000).await;
    set_event_at(&boot, failed_id, 500).await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/retry-regressed-skew.json" }),
    )
    .await
    .expect("planner can read retry-regressed-skew run json");
    let run = content_json(&out);
    assert_eq!(run["status"], json!("failed"));
    assert_eq!(run["finished_at"], json!(500));
    assert_eq!(
        run["events"]["completed"]["payload"]["result"],
        json!({ "summary": "first attempt worked" })
    );
    assert_eq!(
        run["events"]["failed"]["payload"]["reason"],
        json!("retry failed")
    );
}

#[tokio::test]
async fn final_event_same_timestamp_uses_event_id() {
    let boot = boot().await;
    request_codex(&boot, "retry-tie").await;
    materialize_worker(&boot, "retry-tie").await;
    let failed_id = worker_fail_run(&boot, "retry-tie", "same millisecond failure").await;
    let completed_id = complete_run(&boot, "retry-tie", "same millisecond success").await;
    assert!(completed_id > failed_id);
    set_event_at(&boot, failed_id, 100).await;
    set_event_at(&boot, completed_id, 100).await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/retry-tie.json" }),
    )
    .await
    .expect("planner can read retry-tie run json");
    let run = content_json(&out);
    assert_eq!(run["status"], json!("completed"));
    assert_eq!(
        run["events"]["completed"]["payload"]["result"],
        json!({ "summary": "same millisecond success" })
    );
    assert_eq!(
        run["events"]["failed"]["payload"]["reason"],
        json!("same millisecond failure")
    );
}

#[tokio::test]
async fn worker_card_without_request_is_unknown_even_when_materialized() {
    let boot = boot().await;
    let worker_id = materialize_worker(&boot, "orphan-worker").await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/orphan-worker.json" }),
    )
    .await
    .expect("planner can read orphan-worker run json");
    let run = content_json(&out);
    assert_eq!(run["status"], json!("unknown"));
    assert_eq!(run["worker_card_id"], json!(worker_id.as_str()));
    assert!(run["events"]["requested"].is_null(), "run = {run:?}");
}

#[tokio::test]
async fn planner_rejection_without_worker_failure_stays_out_of_failed_pool() {
    let boot = boot().await;
    request_codex(&boot, "reject-only").await;
    reject_run(&boot, "reject-only", "not started").await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/reject-only.json" }),
    )
    .await
    .expect("planner can read reject-only run json");
    let run = content_json(&out);
    assert_eq!(run["status"], json!("requested"));
    assert!(run["events"]["failed"].is_null(), "run = {run:?}");
    assert_eq!(run["verdict"]["status"], json!("rejected"));
    assert_eq!(run["verdict"]["reason"], json!("not started"));
}

#[tokio::test]
async fn rejected_verdict_before_worker_completion_preserves_worker_output() {
    let boot = boot().await;
    request_codex(&boot, "reject-out-of-order").await;
    materialize_worker(&boot, "reject-out-of-order").await;
    reject_run(&boot, "reject-out-of-order", "early rejection").await;
    complete_run(&boot, "reject-out-of-order", "worker arrived later").await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/reject-out-of-order.json" }),
    )
    .await
    .expect("planner can read reject-out-of-order run json");
    let run = content_json(&out);
    assert_eq!(run["status"], json!("completed"));
    assert_eq!(
        run["events"]["completed"]["payload"]["result"],
        json!({ "summary": "worker arrived later" })
    );
    assert!(run["events"]["failed"].is_null(), "run = {run:?}");
    assert_eq!(run["verdict"]["status"], json!("rejected"));
    assert_eq!(run["verdict"]["reason"], json!("early rejection"));
}

#[tokio::test]
async fn verdict_before_worker_completion_still_preserves_worker_output() {
    let boot = boot().await;
    request_codex(&boot, "out-of-order").await;
    materialize_worker(&boot, "out-of-order").await;
    log_track_event(
        &boot,
        &boot.track_id,
        &boot.area_id,
        Event::TaskCompleted {
            idempotency_key: "out-of-order".into(),
            result: json!({ "status": "accepted", "reason": "early LGTM" }),
            artifacts: vec![],
            agent_message: None,
        },
    )
    .await;
    complete_run(&boot, "out-of-order", "worker arrived later").await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/out-of-order.json" }),
    )
    .await
    .expect("planner can read out-of-order run json");
    let run = content_json(&out);
    assert_eq!(run["status"], json!("completed"));
    assert_eq!(
        run["events"]["completed"]["payload"]["result"],
        json!({ "summary": "worker arrived later" })
    );
    assert_eq!(run["verdict"]["status"], json!("accepted"));
    assert_eq!(run["verdict"]["reason"], json!("early LGTM"));
}

async fn assert_reserved_run_key_error(tool: &str, path: &str, expect: &str) {
    let boot = boot().await;
    request_codex(&boot, "index").await;

    let err = call_tool(
        &boot,
        tool,
        planner_identity(&boot),
        json!({ "path": path }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, RpcError::INTERNAL_ERROR);
    assert!(
        err.message
            .contains("idempotency_key `index` collides with reserved path"),
        "{expect}: err = {err:?}"
    );
    assert!(
        err.message.contains("Remediation: stop submitting jobs"),
        "{expect}: err = {err:?}"
    );
}

#[tokio::test]
async fn reserved_run_key_returns_structured_error_for_runs_index_json() {
    assert_reserved_run_key_error(TOOL_TRACK_CAT, "runs/index.json", "cat runs/index.json").await;
}

#[tokio::test]
async fn reserved_run_key_returns_structured_error_for_ls_root() {
    assert_reserved_run_key_error(TOOL_TRACK_LS, "/", "ls /").await;
}

#[tokio::test]
async fn reserved_run_key_returns_structured_error_for_ls_runs() {
    assert_reserved_run_key_error(TOOL_TRACK_LS, "runs/", "ls runs/").await;
}

#[tokio::test]
async fn reserved_run_key_returns_structured_error_for_run_markdown() {
    assert_reserved_run_key_error(TOOL_TRACK_CAT, "runs/index.md", "cat runs/index.md").await;
}

#[tokio::test]
async fn runs_do_not_leak_across_tracks() {
    let boot = boot().await;
    request_codex(&boot, "private-run").await;
    materialize_worker(&boot, "private-run").await;

    let err = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        other_planner_identity(&boot),
        json!({ "path": "runs/private-run.md" }),
    )
    .await
    .expect_err("other track must not see this run");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(
        err.message.contains("path not available in this view"),
        "err = {err:?}"
    );
}

#[tokio::test]
async fn unknown_run_key_matches_unknown_card_error_shape() {
    let boot = boot().await;
    let card_err = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "cards/not-a-card/.payload.json" }),
    )
    .await
    .expect_err("unknown card must be unavailable");
    let run_err = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/not-a-run.md" }),
    )
    .await
    .expect_err("unknown run must be unavailable");
    assert_eq!(run_err.code, card_err.code);
    assert_eq!(run_err.code, RpcError::INVALID_PARAMS);
    assert!(run_err.message.contains("path not available in this view"));
}

#[tokio::test]
async fn run_status_derivation_follows_projection_rules() {
    let boot = boot().await;
    request_codex(&boot, "request-only").await;
    request_codex(&boot, "running-run").await;
    materialize_worker(&boot, "running-run").await;
    request_codex(&boot, "completed-run").await;
    complete_run(&boot, "completed-run", "done").await;
    request_codex(&boot, "failed-run").await;
    worker_fail_run(&boot, "failed-run", "bad exit").await;

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/index.json" }),
    )
    .await
    .expect("planner can read runs index");
    let runs = content_json(&out);
    let runs = runs.as_array().expect("runs index array");
    let status = |key: &str| {
        runs.iter()
            .find(|run| run["idempotency_key"] == key)
            .unwrap_or_else(|| panic!("missing {key}: {runs:?}"))["status"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(status("request-only"), "requested");
    assert_eq!(status("running-run"), "running");
    assert_eq!(status("completed-run"), "completed");
    assert_eq!(status("failed-run"), "failed");
}

#[tokio::test]
async fn empty_track_has_empty_runs_projection() {
    let boot = boot().await;

    let ls = call_tool(
        &boot,
        TOOL_TRACK_LS,
        planner_identity(&boot),
        json!({ "path": "runs/" }),
    )
    .await
    .expect("planner can list runs");
    let runs = ls.as_array().expect("runs ls returns array");
    assert_eq!(runs.len(), 1, "runs = {runs:?}");
    assert_eq!(runs[0]["name"], json!("index.json"));
    assert_eq!(runs[0]["kind"], json!("file"));

    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "runs/index.json" }),
    )
    .await
    .expect("planner can read empty runs index");
    assert_eq!(content_json(&out), json!([]));
}

#[tokio::test]
async fn card_payload_from_other_track_is_forbidden() {
    let boot = boot().await;
    for leaf in [".payload.json", ".meta.json"] {
        let path = format!("cards/{}/{leaf}", boot.other_track_card_id.as_str());
        let err = call_tool(
            &boot,
            TOOL_TRACK_CAT,
            planner_identity(&boot),
            json!({ "path": path }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, -32403);
        assert!(err.message.contains("forbidden"), "err = {err:?}");
    }

    let worker_payload_path = format!("cards/{}/.payload.json", boot.other_track_card_id.as_str());
    let err = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        worker_identity(&boot),
        json!({ "path": worker_payload_path }),
    )
    .await
    .expect_err("worker cross-track card payload read must be denied");
    assert_eq!(err.code, -32403);
    assert!(err.message.contains("forbidden"), "err = {err:?}");

    let path = format!("cards/{}", boot.other_track_card_id.as_str());
    let err = call_tool(
        &boot,
        TOOL_TRACK_LS,
        planner_identity(&boot),
        json!({ "path": path }),
    )
    .await
    .expect_err("cross-track card directory listing must be denied");
    assert_eq!(err.code, -32403);
    assert!(err.message.contains("forbidden"), "err = {err:?}");
}

#[tokio::test]
async fn track_file_tools_allow_worker_bound_track_reads() {
    let boot = boot().await;

    let ls = call_tool(&boot, TOOL_TRACK_LS, worker_identity(&boot), json!({}))
        .await
        .expect("worker can list its bound track");
    assert!(ls.as_array().is_some(), "ls should return an array: {ls:?}");

    let cat = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        worker_identity(&boot),
        json!({ "path": "runs/index.json" }),
    )
    .await
    .expect("worker can read its bound track");
    assert_eq!(content_json(&cat), json!([]));

    let report = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        worker_identity(&boot),
        json!({ "path": "report.md" }),
    )
    .await
    .expect("worker can read its bound track report");
    assert_eq!(report["content_type"], json!("text/markdown"));
    assert_eq!(
        report["content"].as_str(),
        Some(TrackReportPayload::initial().body.as_str())
    );
}

#[tokio::test]
async fn unknown_paths_return_clean_errors() {
    let boot = boot().await;
    let local_bad = format!("cards/{}/nonexistent", boot.worker_card_id.as_str());
    for path in ["runs/anything", "foo", local_bad.as_str()] {
        let err = call_tool(
            &boot,
            TOOL_TRACK_CAT,
            planner_identity(&boot),
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
async fn index_md_uses_bound_track_title_and_id() {
    let boot = boot().await;
    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "index.md" }),
    )
    .await
    .expect("index.md cat works");
    assert_eq!(out["content_type"], json!("text/markdown"));
    let content = out["content"].as_str().expect("markdown content");
    assert!(
        content.starts_with(&format!("# Track {}", boot.track_id.as_str())),
        "index.md should start with the bound track id: {content:?}"
    );
    assert!(
        content.contains("- Title: track file test"),
        "index.md should contain the bound track title: {content:?}"
    );
}

#[tokio::test]
async fn report_md_matches_report_read_body() {
    let boot = boot().await;
    let report = call_tool(&boot, TOOL_REPORT_READ, planner_identity(&boot), json!({}))
        .await
        .expect("report read works");
    let report_body = report["body"].as_str().expect("report body");

    let file = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "report.md" }),
    )
    .await
    .expect("report.md cat works");
    assert_eq!(file["content_type"], json!("text/markdown"));
    assert_eq!(file["content"].as_str(), Some(report_body));
}

#[tokio::test]
async fn hidden_track_history_tools_are_callable_and_patch_report() {
    let boot = boot().await;
    track_vcs::backfill_existing_tracks(boot.sqlx_repo.pool())
        .await
        .expect("backfill VCS baseline");
    let before = track_vcs::head(boot.sqlx_repo.pool(), &boot.track_id)
        .await
        .unwrap()
        .unwrap();

    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        planner_identity(&boot),
        json!({
            "body": "# Report\n\n- visible change\n",
            "summary": "changed",
            "message": "write report for VCS diff",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect("report write works");
    let after = track_vcs::head(boot.sqlx_repo.pool(), &boot.track_id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(before, after);

    let diff = call_tool(
        &boot,
        TOOL_TRACK_DIFF,
        planner_identity(&boot),
        json!({ "from": before.clone(), "to": after.clone(), "path": "report.md" }),
    )
    .await
    .expect("hidden diff is callable");
    let files = diff["files"].as_array().expect("files array");
    assert_eq!(files.len(), 1, "diff = {diff:#?}");
    assert_eq!(files[0]["path"], json!("report.md"));
    assert_eq!(files[0]["status"], json!("modified"));
    let patch = files[0]["patch"].as_str().expect("text patch");
    assert!(patch.contains("--- a/report.md"), "patch = {patch}");
    assert!(patch.contains("+++ b/report.md"), "patch = {patch}");
    assert!(patch.contains("@@"), "patch = {patch}");
    assert!(patch.contains("+# Report"), "patch = {patch}");

    let cat = call_tool(
        &boot,
        TOOL_TRACK_CAT_AT,
        planner_identity(&boot),
        json!({ "commit": after.clone(), "path": "report.md" }),
    )
    .await
    .expect("hidden cat_at is callable");
    assert_eq!(cat["content_type"], json!("text/markdown"));
    assert_eq!(cat["content"], json!("# Report\n\n- visible change\n"));

    let log = call_tool(
        &boot,
        TOOL_TRACK_LOG,
        planner_identity(&boot),
        json!({ "path": "report.md", "limit": 5 }),
    )
    .await
    .expect("hidden log is callable");
    assert_eq!(log["truncated"], json!(false));
    let commits = log["commits"].as_array().expect("commits array");
    assert!(
        commits.iter().any(|commit| commit["changed_paths"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "report.md")),
        "log should include report.md changes: {log:#?}"
    );
}

#[tokio::test]
async fn track_json_uses_bound_track_metadata() {
    let boot = boot().await;
    let out = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "track.json" }),
    )
    .await
    .expect("track.json cat works");
    let track = content_json(&out);
    assert_eq!(track["id"], json!(boot.track_id.as_str()));

    let repo_track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(track["created_at"], json!(repo_track.created_at));
}

// ---------------------------------------------------------------------------
// Issue #644 PR-C — plan/<key>/gate.log (planner-role-gated, file-backed)
// ---------------------------------------------------------------------------

async fn seed_gated_task(boot: &Boot, key: &str, gate_attempt: i64) -> String {
    let task = calm_server::model::Task {
        id: format!("{}:{key}", boot.track_id.as_str()),
        track_id: boot.track_id.as_str().to_string(),
        key: key.to_string(),
        kind: calm_server::model::TaskKind::Codex,
        goal: "g".into(),
        context_json: "null".into(),
        acceptance_criteria: None,
        cwd: None,
        depends_on_json: "[]".into(),
        priority: 0,
        gate_json: Some(json!({ "steps": [{ "name": "t", "cmd": "true" }] }).to_string()),
        status: calm_server::model::TaskStatus::Verifying,
        status_detail: None,
        worker_card_id: None,
        gate_result_json: None,
        gate_attempt,
        gate_pid: None,
        gate_pid_starttime: None,
        gate_pid_boot_id: None,
        running_deadline_ms: None,
        context_stale_at_ms: None,
        declared_by: "spec".into(),
        spawn: "in-wave".into(),
        created_at_ms: now_ms(),
        updated_at_ms: now_ms(),
        finished_at_ms: None,
    };
    let id = task.id.clone();
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            crate::support::task::insert_task_tx(tx, &task).await?;
            Ok(())
        })
    })
    .await
    .expect("seed gated task");
    id
}

#[tokio::test]
async fn gate_log_view_is_planner_only_and_file_backed() {
    use calm_server::track_fs_view::{TrackFsError, TrackFsView};

    let boot = boot().await;
    let dir = std::env::temp_dir().join(format!(
        "neige-gate-log-view-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let task_id = seed_gated_task(&boot, "gated", 2).await;
    std::fs::write(
        dir.join(format!("{task_id}-g2.log")),
        "::gate-step t\ngate-log-body\n",
    )
    .unwrap();
    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let write = boot.ctx.write.clone();

    // Planner role with access wired: reads the CURRENT attempt's log.
    let view = TrackFsView::new(boot.ctx.repo.as_ref(), &write)
        .with_gate_log_access(CardRole::Planner, dir.clone());
    let content = view.cat(&track, "plan/gated/gate.log").await.expect("read");
    assert_eq!(content.content_type, "text/plain");
    assert!(content.content.contains("gate-log-body"), "{content:?}");

    // Worker role: Forbidden (§6.7 — workers never read gate material).
    let view = TrackFsView::new(boot.ctx.repo.as_ref(), &write)
        .with_gate_log_access(CardRole::Worker, dir.clone());
    let err = view
        .cat(&track, "plan/gated/gate.log")
        .await
        .expect_err("worker forbidden");
    assert!(matches!(err, TrackFsError::Forbidden(_)), "{err:?}");

    // Surface without gate-log access (HTTP track routes): Forbidden.
    let view = TrackFsView::new(boot.ctx.repo.as_ref(), &write);
    let err = view
        .cat(&track, "plan/gated/gate.log")
        .await
        .expect_err("unwired surface forbidden");
    assert!(matches!(err, TrackFsError::Forbidden(_)), "{err:?}");

    // No such task / no attempt yet: path not available, not an error
    // leak.
    let view = TrackFsView::new(boot.ctx.repo.as_ref(), &write)
        .with_gate_log_access(CardRole::Planner, dir.clone());
    let err = view
        .cat(&track, "plan/missing/gate.log")
        .await
        .expect_err("missing task");
    assert!(matches!(err, TrackFsError::PathNotAvailable(_)), "{err:?}");
    seed_gated_task(&boot, "fresh", 0).await;
    let err = view
        .cat(&track, "plan/fresh/gate.log")
        .await
        .expect_err("no attempt yet");
    assert!(matches!(err, TrackFsError::PathNotAvailable(_)), "{err:?}");

    // MCP wiring: a worker calling calm.track.cat on the gate log gets
    // the Forbidden mapping (-32403), proving identity.role is threaded.
    let err = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        worker_identity(&boot),
        json!({ "path": "plan/gated/gate.log" }),
    )
    .await
    .expect_err("worker forbidden at the MCP surface");
    assert_eq!(err.code, -32403, "{err:?}");

    // PR #685 F3 — the MCP read serves the CONFIGURED gate-logs dir
    // threaded through `AppContext` (a stand-in for `--data-dir`
    // without `CALM_DATA_DIR`), never an env-recomputed default: the
    // log written into `boot.gate_logs_dir` is what the planner reads.
    std::fs::create_dir_all(&boot.gate_logs_dir).unwrap();
    std::fs::write(
        boot.gate_logs_dir.join(format!("{task_id}-g2.log")),
        "::gate-step t\nconfigured-dir-body\n",
    )
    .unwrap();
    let content = call_tool(
        &boot,
        TOOL_TRACK_CAT,
        planner_identity(&boot),
        json!({ "path": "plan/gated/gate.log" }),
    )
    .await
    .expect("planner reads the gate log over MCP");
    assert!(
        content["content"]
            .as_str()
            .unwrap()
            .contains("configured-dir-body"),
        "MCP must serve the AppContext-configured dir: {content}"
    );
    std::fs::remove_dir_all(&boot.gate_logs_dir).ok();

    std::fs::remove_dir_all(&dir).ok();
}
