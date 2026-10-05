//! `mcp_server::tools::track_report` integration smoke: in-memory `SqlxRepo`, `EventBus`, seeded
//! `CardRoleCache` and a directly constructed `AppContext` driving the report tool handlers as plain async fns.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use crate::support::mcp::set_persisted_card_role;
use crate::support::report_writes;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::RepoEventWrite;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_insert_tx, session_mark_track_root_tx};
use calm_server::error::CalmError;
use calm_server::event::{EditAuthor, Event, EventBus, EventScope};
use calm_server::ids::{AreaId, CardId, TrackId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::track_report::TOOL_REPORT_READ;
use calm_server::mcp_server::tools::track_report_blocks::{TOOL_REPORT_COMMIT, TOOL_REPORT_WRITE};
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::session_projection_repo::AgentProvider;
use calm_server::track_report::TrackReportPayload;
use calm_types::worker::{
    LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession, WorkerSessionId,
    WorkerSessionState,
};
use serde_json::{Value, json};

const PLANNER_SESSION_ID: &str = "planner-session";
/// The assistant's session is deliberately not the track root; it is bound to its own `CardRole::Assistant` card.
pub(crate) const ASSISTANT_SESSION_ID: &str = "assistant-session";
/// A second, independent assistant conversation on the same track: its own `CardRole::Assistant` card and its own non-root session.
pub(crate) const ASSISTANT_B_SESSION_ID: &str = "assistant-b-session";
pub(crate) const WORKER_SESSION_ID: &str = "worker-session";

/// In-memory fixture: one area → one track → one planner card + one track-report card + one worker card.
pub(crate) struct Boot {
    pub(crate) ctx: Arc<AppContext>,
    pub(crate) registry: Arc<ToolRegistry>,
    pub(crate) repo: Arc<dyn Repo>,
    pub(crate) area_id: AreaId,
    pub(crate) track_id: TrackId,
    pub(crate) planner_card_id: CardId,
    pub(crate) report_card_id: CardId,
    pub(crate) worker_card_id: CardId,
    pub(crate) assistant_card_id: CardId,
    pub(crate) assistant_b_card_id: CardId,
    /// Shares state with the `CardRoleCache` inside [`Boot::ctx`]; a card minted after `boot()` must be seeded into it via `repo.seed_card_role_cache(&cache)`.
    pub(crate) card_role_cache: CardRoleCache,
}

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

pub(crate) async fn seed_track_root_session(
    repo: &dyn RepoEventWrite,
    track_id: &TrackId,
    card_id: &CardId,
    session_id: &str,
) {
    let session = planner_session(session_id, track_id.clone(), card_id.clone());
    let root_session_id = session.id.clone();
    let track_id = track_id.clone();
    calm_server::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            session_insert_tx(tx, session)
                .await
                .map_err(CalmError::from)?;
            session_mark_track_root_tx(tx, &track_id, &root_session_id)
                .await
                .map_err(CalmError::from)?;
            Ok(())
        })
    })
    .await
    .expect("seed track root session");
}

/// A live, card-bound, non-root session row. Contract must be `Executor`: an active `Planner`-contract
/// session would repoint `tracks.root_session_id` and steal the track root from the planner card.
async fn seed_non_root_session(
    repo: &dyn RepoEventWrite,
    track_id: &TrackId,
    card_id: &CardId,
    session_id: &str,
) {
    seed_non_root_session_with_provider(
        repo,
        track_id,
        card_id,
        session_id,
        WorkerProviderKind::Codex,
    )
    .await;
}

/// [`seed_non_root_session`] with `worker_sessions.provider` spelled out. The column does not pick the
/// actor arm — `ToolCallIdentity::provider` does; `call_tool` bypasses the transport hop that would derive it.
pub(crate) async fn seed_non_root_session_with_provider(
    repo: &dyn RepoEventWrite,
    track_id: &TrackId,
    card_id: &CardId,
    session_id: &str,
    provider: WorkerProviderKind,
) {
    let mut session = planner_session(session_id, track_id.clone(), card_id.clone());
    session.contract = WorkerContract::Executor;
    session.provider = provider;
    calm_server::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            session_insert_tx(tx, session)
                .await
                .map_err(CalmError::from)?;
            Ok(())
        })
    })
    .await
    .expect("seed non-root session");
}

pub(crate) async fn boot() -> Boot {
    boot_at("sqlite::memory:").await
}

/// [`boot`] on an explicit sqlite URL (a file-backed database an out-of-process kernel can be
/// launched against afterwards).
pub(crate) async fn boot_at(db_url: &str) -> Boot {
    boot_with_scope(db_url, None).await
}

pub(crate) async fn boot_development() -> Boot {
    const ID: &str = "dev.neige.git-forge";
    let boot = boot_with_scope("sqlite::memory:", Some(ID)).await;
    let host = Arc::new(calm_server::plugin_host::PluginHost::new_full(
        Arc::new(calm_server::plugin_host::PluginRegistry::empty().with_builtins()),
        boot.repo.clone(),
        boot.ctx.gate_logs_dir.join("plugins"),
        boot.ctx.gate_logs_dir.join("plugin-data"),
        Vec::new(),
        boot.ctx.events.clone(),
        boot.ctx.write.clone(),
    ));
    host.reconcile_builtins().await.unwrap();
    host.enable(ID).await.unwrap();
    assert!(boot.ctx.plugin_host.set(host).is_ok());
    boot
}

async fn boot_with_scope(db_url: &str, plugin_scope: Option<&str>) -> Boot {
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open(db_url).await.expect("open sqlite"));
    let area = repo
        .area_create(NewArea {
            name: "report-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "report track".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: plugin_scope.map(str::to_string),
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
            sort: None,
            payload: serde_json::json!({"planner_provider": "codex"}),
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
    let worker_card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .unwrap();
    // The payload carries the `harness_profile` marker; without it a card is invisible to the track
    // conversation list and cannot receive a message.
    let assistant_card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "harness_profile": "assistant"}),
        })
        .await
        .unwrap();
    let assistant_b_card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "harness_profile": "assistant"}),
        })
        .await
        .unwrap();
    set_persisted_card_role(repo.as_ref(), planner_card.id.as_str(), CardRole::Planner).await;
    set_persisted_card_role(
        repo.as_ref(),
        assistant_card.id.as_str(),
        CardRole::Assistant,
    )
    .await;
    set_persisted_card_role(
        repo.as_ref(),
        assistant_b_card.id.as_str(),
        CardRole::Assistant,
    )
    .await;
    set_persisted_card_role(repo.as_ref(), worker_card.id.as_str(), CardRole::Worker).await;
    seed_track_root_session(
        repo.as_ref(),
        &track.id,
        &planner_card.id,
        PLANNER_SESSION_ID,
    )
    .await;
    seed_non_root_session(
        repo.as_ref(),
        &track.id,
        &assistant_card.id,
        ASSISTANT_SESSION_ID,
    )
    .await;
    seed_non_root_session(
        repo.as_ref(),
        &track.id,
        &assistant_b_card.id,
        ASSISTANT_B_SESSION_ID,
    )
    .await;
    seed_non_root_session(repo.as_ref(), &track.id, &worker_card.id, WORKER_SESSION_ID).await;

    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    card_role_cache.insert(planner_card.id.clone(), CardRole::Planner, track.id.clone());
    card_role_cache.insert(
        assistant_card.id.clone(),
        CardRole::Assistant,
        track.id.clone(),
    );
    card_role_cache.insert(
        assistant_b_card.id.clone(),
        CardRole::Assistant,
        track.id.clone(),
    );
    card_role_cache.insert(
        report_card.id.clone(),
        CardRole::ReportCard,
        track.id.clone(),
    );
    card_role_cache.insert(worker_card.id.clone(), CardRole::Worker, track.id.clone());

    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();
    let ctx = Arc::new(AppContext {
        terminal_interaction: Arc::new(tokio::sync::OnceCell::new()),
        repo: route_repo,
        track_vcs: repo
            .sqlite_pool()
            .map(calm_truth::track_vcs_repo::SqlxTrackVcsRepo::shared),
        events,
        write: calm_server::state::WriteContext::new(card_role_cache.clone(), track_area_cache),
        daemon_token_hash: None,
        gate_logs_dir: std::env::temp_dir().join("neige-test-gate-logs"),
        plugin_host: Arc::new(tokio::sync::OnceCell::new()),
        operation_runtime: Arc::new(tokio::sync::OnceCell::new()),
        track_creator: Arc::new(tokio::sync::OnceCell::new()),
        scheduler_poke: Arc::new(tokio::sync::OnceCell::new()),
        // Unstarted: reads record their `enqueue` outcomes and the series tests run the recorded jobs by hand.
        series_resolver: Arc::new(calm_server::report_series::SeriesResolver::new_unstarted(
            repo.sqlite_pool(),
        )),
        plugin_results: Arc::new(calm_server::plugin_results::PluginResults::new()),
        read_ledger: Arc::new(calm_server::report_read_ledger::ReadLedger::new()),
        preview: Arc::new(calm_server::preview::PreviewRegistry::disabled()),
        sqlite_pool: repo.sqlite_pool(),
    });

    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);
    let registry = Arc::new(registry);

    Boot {
        ctx,
        registry,
        repo,
        area_id: area.id,
        track_id: track.id,
        planner_card_id: planner_card.id,
        report_card_id: report_card.id,
        worker_card_id: worker_card.id,
        assistant_card_id: assistant_card.id,
        assistant_b_card_id: assistant_b_card.id,
        card_role_cache,
    }
}

pub(crate) async fn call_tool(
    boot: &Boot,
    name: &str,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let handler = boot
        .registry
        .lookup(name)
        .unwrap_or_else(|| panic!("tool not registered: {name}"));
    handler(boot.ctx.clone(), identity, args)
        .await
        .map(calm_server::mcp_server::result::ToolResult::into_structured)
}

/// The wire shape of a tool result (`content` + `structuredContent`); `call_tool` projects to `structuredContent` and hides the text block.
pub(crate) async fn call_tool_raw(
    boot: &Boot,
    name: &str,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let handler = boot
        .registry
        .lookup(name)
        .unwrap_or_else(|| panic!("tool not registered: {name}"));
    handler(boot.ctx.clone(), identity, args)
        .await
        .map(|result| serde_json::to_value(&result).expect("serialize tool result"))
}

/// [`report_writes::upsert_block`] on this fixture.
pub(crate) async fn upsert_block(
    boot: &Boot,
    identity: ToolCallIdentity,
    op: Value,
) -> Result<Value, RpcError> {
    report_writes::upsert_block(&boot.ctx, &boot.registry, identity, op).await
}

/// [`report_writes::read_then_commit`] on this fixture.
pub(crate) async fn read_then_commit(
    boot: &Boot,
    identity: ToolCallIdentity,
    ops: Value,
) -> Result<Value, RpcError> {
    report_writes::read_then_commit(&boot.ctx, &boot.registry, identity, ops).await
}

/// [`report_writes::read_then_write_markdown`] on this fixture.
pub(crate) async fn read_then_write_markdown(
    boot: &Boot,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    report_writes::read_then_write_markdown(&boot.ctx, &boot.registry, identity, args).await
}

pub(crate) async fn current_doc_rev(boot: &Boot) -> u64 {
    calm_server::track_report_read::load_report_read_snapshot(
        boot.repo.as_ref(),
        boot.report_card_id.as_str(),
    )
    .await
    .expect("read current document revision")
    .doc_rev
}

pub(crate) fn planner_identity(boot: &Boot) -> ToolCallIdentity {
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

/// A `CardRole::Assistant` caller on this track: its own card, its own non-root session.
pub(crate) fn assistant_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.assistant_card_id.as_str().to_string(),
        role: CardRole::Assistant,
        provider: AgentProvider::Codex,
        session_id: ASSISTANT_SESSION_ID.to_string(),
        track_id: Some(boot.track_id.as_str().to_string()),
        area_id: boot.area_id.as_str().to_string(),
        thread_id: "assistant-thread".to_string(),
    }
}

/// The other assistant conversation on this same track: a distinct `CardRole::Assistant` card and a
/// distinct live non-root session. Token issuance and the transport's token → identity binding are out of frame.
pub(crate) fn assistant_b_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.assistant_b_card_id.as_str().to_string(),
        role: CardRole::Assistant,
        provider: AgentProvider::Codex,
        session_id: ASSISTANT_B_SESSION_ID.to_string(),
        track_id: Some(boot.track_id.as_str().to_string()),
        area_id: boot.area_id.as_str().to_string(),
        thread_id: "assistant-b-thread".to_string(),
    }
}

pub(crate) fn worker_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.worker_card_id.as_str().to_string(),
        role: CardRole::Worker,
        provider: AgentProvider::Codex,
        session_id: WORKER_SESSION_ID.to_string(),
        track_id: Some(boot.track_id.as_str().to_string()),
        area_id: boot.area_id.as_str().to_string(),
        thread_id: "worker-thread".to_string(),
    }
}

pub(crate) async fn collect_n(
    events: &EventBus,
    n: usize,
) -> Vec<calm_server::event::BroadcastEnvelope> {
    let mut sub = events.subscribe();
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        match tokio::time::timeout(Duration::from_secs(2), sub.recv()).await {
            Ok(Ok(env)) => out.push(env),
            Ok(Err(_lag)) => break,
            Err(_timeout) => break,
        }
    }
    out
}

async fn recv_env(
    rx: &mut tokio::sync::broadcast::Receiver<calm_server::event::BroadcastEnvelope>,
) -> calm_server::event::BroadcastEnvelope {
    tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("bus delivers within timeout")
        .expect("bus open")
}

/// `neige_report_commit` arguments replacing the report's only block with `markdown` — the planner's
/// local edit, anchored by the planner's read here.
async fn commit_replacing_only_block(boot: &Boot, markdown: &str, message: &str) -> Value {
    let read = call_tool(boot, TOOL_REPORT_READ, planner_identity(boot), json!({}))
        .await
        .expect("planner reads the report");
    let blocks = read["blocks"].as_array().expect("blocks");
    assert_eq!(blocks.len(), 1, "the fixture report has one block: {read}");
    json!({
        "message": message,
        "ops": [{
            "op": "upsert",
            "id": blocks[0]["id"],
            "kind": "prose",
            "markdown": markdown
        }]
    })
}

#[tokio::test]
async fn read_returns_initial_seeded_body() {
    let boot = boot().await;
    let out = call_tool(&boot, TOOL_REPORT_READ, planner_identity(&boot), json!({}))
        .await
        .expect("planner can read the report");
    assert_eq!(
        out.get("text").and_then(Value::as_str),
        Some(TrackReportPayload::initial().body.as_str())
    );
    assert_eq!(out.get("summary").and_then(Value::as_str), Some(""));
    assert_eq!(out.get("schemaVersion").and_then(Value::as_u64), Some(4));
    assert_eq!(out.get("docRev").and_then(Value::as_u64), Some(0));
    assert!(
        out.get("updated_at").and_then(Value::as_i64).unwrap_or(0) > 0,
        "updated_at is a positive timestamp; got {out:?}",
    );
}

#[tokio::test]
async fn read_refuses_worker() {
    let boot = boot().await;
    let err = call_tool(&boot, TOOL_REPORT_READ, worker_identity(&boot), json!({}))
        .await
        .expect_err("worker must be denied");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("Planner"), "msg = {err:?}");
}

#[tokio::test]
async fn whole_document_write_requires_a_read_and_rejects_stale_planner_writer() {
    let boot = boot().await;
    let missing = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        planner_identity(&boot),
        json!({"body": "# A\n", "message": "no read"}),
    )
    .await
    .unwrap_err();
    assert_eq!(missing.code, -32602);

    call_tool(&boot, TOOL_REPORT_READ, planner_identity(&boot), json!({}))
        .await
        .unwrap();
    read_then_write_markdown(
        &boot,
        assistant_identity(&boot),
        json!({"body": "# First\n", "message": "first writer"}),
    )
    .await
    .unwrap();
    let conflict = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        planner_identity(&boot),
        json!({"body": "# Stale\n", "message": "second writer"}),
    )
    .await
    .unwrap_err();
    assert_eq!(conflict.code, -32001);
    assert!(conflict.message.contains("current doc_rev is 1"));
    assert!(conflict.message.contains("this session last read docRev 0"));
    assert!(conflict.message.contains("re-read"));
    let read = call_tool(&boot, TOOL_REPORT_READ, planner_identity(&boot), json!({}))
        .await
        .unwrap();
    assert_eq!(read["text"], "# First\n", "stale writer must not win");
}

#[tokio::test]
async fn write_markdown_replaces_body_and_emits_card_updated() {
    let boot = boot().await;
    let events = boot.ctx.events.clone();
    let report_id = boot.report_card_id.clone();
    let track_id = boot.track_id.clone();
    let sub = tokio::spawn(async move { collect_n(&events, 2).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let out = read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({
            "body": "# Goal\n\nrefactored everything\n",
            "summary": "done refactoring",
            "message": "rewrite report"
        }),
    )
    .await
    .expect("planner writes successfully");
    let new_updated_at = out
        .get("updated_at")
        .and_then(Value::as_i64)
        .expect("updated_at i64");
    assert_eq!(out.get("docRev").and_then(Value::as_u64), Some(1));

    let envs = sub.await.expect("collector ok");
    assert_eq!(
        envs.len(),
        2,
        "expected exactly two envelopes; got {envs:?}"
    );

    match &envs[0].event {
        Event::CardUpdated(c) => {
            assert_eq!(c.id, report_id, "envelope is for the report card");
            assert_eq!(c.kind, "track-report");
            let payload: TrackReportPayload =
                serde_json::from_value(c.payload.clone()).expect("payload deserializes");
            assert_eq!(payload.body, "# Goal\n\nrefactored everything\n");
            assert_eq!(payload.summary, "done refactoring");
            assert_eq!(payload.schema_version, 4);
            assert_eq!(payload.doc_rev, 1);
            assert_eq!(c.updated_at, new_updated_at);
        }
        other => panic!("expected CardUpdated first, got {other:?}"),
    }
    assert!(matches!(envs[0].scope, EventScope::Card { .. }));

    match &envs[1].event {
        Event::TrackReportEdited {
            track_id: w,
            card_id: c,
            author,
            author_plugin_id: _,
            edit_id,
            summary_before,
            summary_after,
            body_before,
            body_after,
            agent_message,
        } => {
            assert_eq!(w, &track_id, "track_id matches the report card's track");
            assert_eq!(c, &report_id, "card_id matches the report card");
            assert_eq!(*author, EditAuthor::Planner, "MCP path tags Planner");
            assert_eq!(agent_message.as_deref(), Some("rewrite report"));
            assert!(!edit_id.is_empty(), "edit_id must be a non-empty UUID");
            // UUID v4 string is 36 chars (8-4-4-4-12 with hyphens).
            assert_eq!(
                edit_id.len(),
                36,
                "edit_id should be a UUID v4 string; got {edit_id:?}",
            );
            assert_eq!(
                summary_before, "",
                "pre-write summary is the empty initial value",
            );
            assert_eq!(
                body_before,
                &TrackReportPayload::initial().body,
                "pre-write body is the initial seed body",
            );
            assert_eq!(summary_after, "done refactoring");
            assert_eq!(body_after, "# Goal\n\nrefactored everything\n");
        }
        other => panic!("expected TrackReportEdited second, got {other:?}"),
    }
    // The scope row must also populate `scope_track` + `scope_card` for the dispatcher's push filter.
    match &envs[1].scope {
        EventScope::Card { card, track, .. } => {
            assert_eq!(card, &report_id, "scope_card persisted on the events row");
            assert_eq!(track, &track_id, "scope_track persisted on the events row");
        }
        other => panic!("expected Card-scoped envelope, got {other:?}"),
    }

    let card = boot
        .repo
        .card_get(report_id.as_str())
        .await
        .unwrap()
        .expect("report card row");
    let payload: TrackReportPayload =
        serde_json::from_value(card.payload).expect("payload deserializes");
    assert_eq!(payload.body, "# Goal\n\nrefactored everything\n");
}

#[tokio::test]
async fn commit_requires_non_empty_message() {
    let boot = boot().await;

    let err = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        json!({ "summary": "missing message"}),
    )
    .await
    .expect_err("missing message must be rejected");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(
        err.message.contains("message must be non-empty"),
        "msg = {err:?}"
    );

    let err = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        json!({ "summary": "empty message", "message": "\n\t "}),
    )
    .await
    .expect_err("empty message must be rejected");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(
        err.message.contains("message must be non-empty"),
        "msg = {err:?}"
    );
}

#[tokio::test]
async fn write_markdown_records_agent_message_and_leaves_the_track_open() {
    let boot = boot().await;
    let mut rx = boot.ctx.events.subscribe();

    read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({
            "body": "plain body\n",
            "message": "plain write"
        }),
    )
    .await
    .expect("write succeeds");

    let card_env = recv_env(&mut rx).await;
    assert!(matches!(card_env.event, Event::CardUpdated(_)));
    let report_env = recv_env(&mut rx).await;
    match &report_env.event {
        Event::TrackReportEdited { agent_message, .. } => {
            assert_eq!(agent_message.as_deref(), Some("plain write"))
        }
        other => panic!("expected TrackReportEdited, got {other:?}"),
    }
    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert!(track.is_open());
    let no_more = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
    assert!(no_more.is_err(), "unexpected event: {no_more:?}");
}

/// The one refusal every write tool gives a `lifecycle` key.
fn assert_lifecycle_removed(message: &str) {
    assert!(
        message.contains(
            "`lifecycle` is removed: close with neige_track_close; ask with \
             neige_user_notify or neige_ratify_request"
        ),
        "{message}"
    );
}

#[tokio::test]
async fn write_markdown_with_a_lifecycle_key_is_refused_and_writes_nothing() {
    let boot = boot().await;
    let before_track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let before_card = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let mut rx = boot.ctx.events.subscribe();

    let err = read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({
            "body": "should rollback\n",
            "message": "illegal report lifecycle",
            "lifecycle": "done"
        }),
    )
    .await
    .expect_err("a lifecycle key is refused");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert_lifecycle_removed(&err.message);

    let after_track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after_track.closed_at, before_track.closed_at);
    let after_card = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after_card.payload, before_card.payload);
    let no_event = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
    assert!(
        no_event.is_err(),
        "a refused write emitted an event: {no_event:?}"
    );
}

#[tokio::test]
async fn commit_emits_track_report_edited_alongside_card_updated() {
    let boot = boot().await;
    read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({
            "body": "before XYZ after\n",
            "summary": "before-summary",
            "message": "seed report"
        }),
    )
    .await
    .expect("seed write");

    let events = boot.ctx.events.clone();
    let report_id = boot.report_card_id.clone();
    let track_id = boot.track_id.clone();
    let sub = tokio::spawn(async move { collect_n(&events, 2).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let args = commit_replacing_only_block(&boot, "before ABC after\n", "edit report").await;
    call_tool(&boot, TOOL_REPORT_COMMIT, planner_identity(&boot), args)
        .await
        .expect("commit succeeds");

    let envs = sub.await.expect("collector ok");
    assert_eq!(
        envs.len(),
        2,
        "expected CardUpdated + TrackReportEdited; got {envs:?}",
    );
    assert!(
        matches!(envs[0].event, Event::CardUpdated(_)),
        "CardUpdated first",
    );
    match &envs[1].event {
        Event::TrackReportEdited {
            track_id: w,
            card_id: c,
            author,
            author_plugin_id: _,
            edit_id,
            summary_before,
            summary_after,
            body_before,
            body_after,
            agent_message,
        } => {
            assert_eq!(w, &track_id);
            assert_eq!(c, &report_id);
            assert_eq!(*author, EditAuthor::Planner);
            assert_eq!(agent_message.as_deref(), Some("edit report"));
            assert_eq!(edit_id.len(), 36, "edit_id is a UUID v4 string");
            assert_eq!(summary_before, "before-summary");
            assert_eq!(summary_after, "before-summary");
            assert_eq!(body_before, "before XYZ after\n");
            assert_eq!(body_after, "before ABC after\n");
        }
        other => panic!("expected TrackReportEdited, got {other:?}"),
    }
}

#[tokio::test]
async fn write_markdown_with_unchanged_content_still_emits_track_report_edited() {
    let boot = boot().await;
    read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({
            "body": "stable body\n",
            "summary": "stable summary",
            "message": "first stable report"
        }),
    )
    .await
    .expect("first write");
    let first_payload: TrackReportPayload = serde_json::from_value(
        boot.repo
            .card_get(boot.report_card_id.as_str())
            .await
            .unwrap()
            .expect("report after first write")
            .payload,
    )
    .expect("first payload");
    let first_ids: Vec<String> = first_payload
        .blocks
        .expect("derived blocks after first write")
        .into_iter()
        .map(|block| block.id)
        .collect();

    let events = boot.ctx.events.clone();
    let sub = tokio::spawn(async move { collect_n(&events, 2).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({
            "body": "stable body\n",
            "summary": "stable summary",
            "message": "second stable report"
        }),
    )
    .await
    .expect("second write (content-equal)");
    let second_payload: TrackReportPayload = serde_json::from_value(
        boot.repo
            .card_get(boot.report_card_id.as_str())
            .await
            .unwrap()
            .expect("report after second write")
            .payload,
    )
    .expect("second payload");
    let second_ids: Vec<String> = second_payload
        .blocks
        .expect("derived blocks after second write")
        .into_iter()
        .map(|block| block.id)
        .collect();
    assert_eq!(second_ids, first_ids, "content-equal writes preserve ids");

    let envs = sub.await.expect("collector ok");
    assert_eq!(
        envs.len(),
        2,
        "content-equal write still produces both events; got {envs:?}",
    );
    assert!(matches!(envs[0].event, Event::CardUpdated(_)));
    match &envs[1].event {
        Event::TrackReportEdited {
            summary_before,
            summary_after,
            body_before,
            body_after,
            ..
        } => {
            assert_eq!(
                summary_before, summary_after,
                "content-equal write: before == after on summary",
            );
            assert_eq!(
                body_before, body_after,
                "content-equal write: before == after on body",
            );
            assert_eq!(body_before, "stable body\n");
            assert_eq!(summary_before, "stable summary");
        }
        other => panic!("expected TrackReportEdited, got {other:?}"),
    }
}

#[tokio::test]
async fn track_report_edited_persisted_with_track_and_card_scope_columns() {
    let boot = boot().await;
    read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({
            "body": "scoped body\n",
            "summary": "scoped summary",
            "message": "scoped report"
        }),
    )
    .await
    .expect("write succeeds");

    // `events_since` reconstructs scope from the `events.scope_*` columns, so this round-trip asserts what was persisted.
    let cursor_rows = boot.repo.events_since(0, 1000).await.expect("events_since");
    let edited_rows: Vec<_> = cursor_rows
        .iter()
        .filter(|(_id, _ver, _scope, ev)| matches!(ev, Event::TrackReportEdited { .. }))
        .collect();
    assert_eq!(
        edited_rows.len(),
        1,
        "exactly one TrackReportEdited row persisted; got {edited_rows:?}",
    );
    let (_id, _ver, scope, ev) = edited_rows[0];
    match scope {
        EventScope::Card { card, track, area } => {
            assert_eq!(card, &boot.report_card_id, "scope_card");
            assert_eq!(track, &boot.track_id, "scope_track");
            assert!(!area.as_str().is_empty(), "scope_area populated");
        }
        other => panic!("expected Card-scoped row, got {other:?}"),
    }
    match ev {
        Event::TrackReportEdited {
            author,
            author_plugin_id: _,
            body_before,
            body_after,
            summary_after,
            ..
        } => {
            assert_eq!(*author, EditAuthor::Planner);
            assert_eq!(body_before, &TrackReportPayload::initial().body);
            assert_eq!(body_after, "scoped body\n");
            assert_eq!(summary_after, "scoped summary");
        }
        other => panic!("expected TrackReportEdited payload, got {other:?}"),
    }
}

#[tokio::test]
async fn historical_task_context_advanced_payload_survives_events_since() {
    let boot = boot().await;
    sqlx::query(
        "INSERT INTO events(kind,payload,actor,at,event_version,scope_kind,scope_track) VALUES('task.context_advanced',?1,?2,1,12,'track',?3)",
    )
    .bind(json!({"task_id":"historical-task","verdict":"material"}).to_string())
    .bind(serde_json::to_string(&calm_server::ids::ActorId::Kernel).unwrap())
    .bind(boot.track_id.as_str())
    .execute(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    let rows = boot.repo.events_since(0, i64::MAX).await.unwrap();
    assert!(rows.iter().any(|(_, _, _, event)| matches!(
        event,
        Event::TaskContextAdvanced { task_id, track_id, task_key, changed_refs, rationale, .. }
            if task_id == "historical-task"
                && track_id.as_str().is_empty()
                && task_key.is_empty()
                && changed_refs.is_empty()
                && rationale.is_empty()
    )));
}

#[tokio::test]
async fn write_markdown_preserves_summary_when_omitted() {
    let boot = boot().await;
    read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({
            "body": "a",
            "summary": "preserved",
            "message": "set summary"
        }),
    )
    .await
    .unwrap();
    read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({ "body": "b", "message": "preserve summary"}),
    )
    .await
    .unwrap();

    let card = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let payload: TrackReportPayload = serde_json::from_value(card.payload).unwrap();
    assert_eq!(payload.body, "b");
    assert_eq!(payload.summary, "preserved");
}

#[tokio::test]
async fn write_markdown_rejects_missing_body() {
    let boot = boot().await;
    let err = read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({ "summary": "no body", "message": "missing body"}),
    )
    .await
    .expect_err("missing body must be rejected");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("body"), "msg = {err:?}");
}

#[tokio::test]
async fn commit_records_agent_message_and_leaves_the_track_open() {
    let boot = boot().await;
    read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({
            "body": "before XYZ after\n",
            "message": "seed edit no lifecycle"
        }),
    )
    .await
    .expect("seed body");
    let mut rx = boot.ctx.events.subscribe();

    let args =
        commit_replacing_only_block(&boot, "before ABC after\n", "edit without lifecycle").await;
    call_tool(&boot, TOOL_REPORT_COMMIT, planner_identity(&boot), args)
        .await
        .expect("commit succeeds");

    assert!(matches!(
        recv_env(&mut rx).await.event,
        Event::CardUpdated(_)
    ));
    match recv_env(&mut rx).await.event {
        Event::TrackReportEdited {
            agent_message,
            body_after,
            ..
        } => {
            assert_eq!(agent_message.as_deref(), Some("edit without lifecycle"));
            assert_eq!(body_after, "before ABC after\n");
        }
        other => panic!("expected TrackReportEdited, got {other:?}"),
    }
    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert!(track.is_open());
    let no_more = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
    assert!(no_more.is_err(), "unexpected event: {no_more:?}");
}

#[tokio::test]
async fn commit_with_a_lifecycle_key_is_refused_and_writes_nothing() {
    let boot = boot().await;
    read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({
            "body": "before XYZ after\n",
            "message": "seed illegal edit"
        }),
    )
    .await
    .expect("seed body");
    let before_track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let before_card = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let mut rx = boot.ctx.events.subscribe();

    let mut args =
        commit_replacing_only_block(&boot, "before ABC after\n", "illegal edit lifecycle").await;
    args["lifecycle"] = json!("done");
    let err = call_tool(&boot, TOOL_REPORT_COMMIT, planner_identity(&boot), args)
        .await
        .expect_err("a lifecycle key is refused");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert_lifecycle_removed(&err.message);

    let after_track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after_track.closed_at, before_track.closed_at);
    let after_card = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after_card.payload, before_card.payload);
    let no_event = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
    assert!(
        no_event.is_err(),
        "a refused write emitted an event: {no_event:?}"
    );
}

#[tokio::test]
#[allow(deprecated)]
async fn planner_from_different_track_cannot_reach_this_track_report() {
    let boot = boot().await;

    let area2 = boot
        .repo
        .area_create(NewArea {
            name: "track-b".into(),
            color: "#0f0".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track2 = boot
        .repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area2.id.clone(),
            title: "track 2".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let planner2 = boot
        .repo
        .card_create(NewCard {
            track_id: track2.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .unwrap();
    let report2 = boot
        .repo
        .card_create(NewCard {
            track_id: track2.id.clone(),
            title: None,
            kind: "track-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
        })
        .await
        .unwrap();
    set_persisted_card_role(boot.repo.as_ref(), planner2.id.as_str(), CardRole::Planner).await;
    seed_track_root_session(
        boot.repo.as_ref(),
        &track2.id,
        &planner2.id,
        "planner2-session",
    )
    .await;
    boot.ctx
        .write
        .role_cache()
        .insert(planner2.id.clone(), CardRole::Planner, track2.id.clone());

    let planner2_identity = ToolCallIdentity {
        card_id: planner2.id.as_str().to_string(),
        role: CardRole::Planner,
        provider: AgentProvider::Codex,
        session_id: "planner2-session".to_string(),
        track_id: Some(track2.id.as_str().to_string()),
        area_id: area2.id.as_str().to_string(),
        thread_id: "planner2-thread".to_string(),
    };
    read_then_write_markdown(
        &boot,
        planner2_identity,
        json!({
            "body": "track 2 only\n",
            "summary": "track 2",
            "message": "track 2 report"
        }),
    )
    .await
    .expect("planner2 writes its own track's report");

    let card1 = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let payload1: TrackReportPayload = serde_json::from_value(card1.payload).unwrap();
    assert_eq!(
        payload1.body,
        TrackReportPayload::initial().body,
        "track 1's report is the original seed body — cross-track isolation held",
    );

    let card2 = boot
        .repo
        .card_get(report2.id.as_str())
        .await
        .unwrap()
        .unwrap();
    let payload2: TrackReportPayload = serde_json::from_value(card2.payload).unwrap();
    assert_eq!(payload2.body, "track 2 only\n");
    assert_eq!(payload2.summary, "track 2");

    let _ = boot.track_id.clone();
}

/// A prose body of at least 80 KB (three blocks), written through the
/// planner's whole-document write.
pub(crate) async fn seed_large_body(boot: &Boot) -> String {
    let paragraph = "lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(40);
    let body = format!(
        "# One\n\n{paragraph}\n\n# Two\n\n{paragraph}\n\n# Three\n\n{paragraph}\n",
        paragraph = (0..12)
            .map(|i| format!("para {i}: {paragraph}"))
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    assert!(
        body.len() >= 80 * 1024,
        "fixture body is {} bytes",
        body.len()
    );
    read_then_write_markdown(
        boot,
        planner_identity(boot),
        json!({
            "body": body,
            "summary": "a large report",
            "message": "seed large body"
        }),
    )
    .await
    .expect("planner writes the large body");
    body
}

#[tokio::test]
async fn full_read_delivers_the_document_once_behind_a_one_line_summary() {
    let boot = boot().await;
    let body = seed_large_body(&boot).await;
    let wire = call_tool_raw(&boot, TOOL_REPORT_READ, planner_identity(&boot), json!({}))
        .await
        .expect("planner reads the report");
    let content = wire["content"].as_array().expect("content array");
    assert_eq!(content.len(), 1, "{wire}");
    let line = content[0]["text"].as_str().expect("text block");
    assert!(
        line.len() < 300,
        "content[0].text must be a one-line summary, got {} bytes: {line}",
        line.len()
    );
    assert!(!line.contains('\n'), "{line}");
    assert!(
        line.ends_with("; full state in structuredContent"),
        "{line}"
    );
    let doc_rev = current_doc_rev(&boot).await;
    assert!(
        line.starts_with(&format!("docRev {doc_rev} · 3 blocks · ")),
        "{line}"
    );
    assert!(
        line.contains(&format!(" · {} bytes · a large report;", body.len())),
        "{line}"
    );
    let structured = &wire["structuredContent"];
    assert!(
        structured.get("body").is_none(),
        "the legacy `body` alias must be gone: {}",
        structured
            .as_object()
            .map(|o| o.keys().cloned().collect::<Vec<_>>().join(","))
            .unwrap_or_default()
    );
    assert_eq!(structured["text"].as_str(), Some(body.as_str()));
    assert_eq!(structured["docRev"].as_u64(), Some(doc_rev));
    assert_eq!(structured["blocks"].as_array().map(Vec::len), Some(3));
    assert!(structured.get("taskDiagnostics").is_some(), "{structured}");
}

/// The receipt's summary clip is a byte budget on a char boundary; a CJK summary clipped by chars would blow the size bound.
#[tokio::test]
async fn full_read_summary_line_stays_short_for_a_long_cjk_summary() {
    let boot = boot().await;
    let body = seed_large_body(&boot).await;
    let summary = "报".repeat(200);
    call_tool(&boot, TOOL_REPORT_READ, planner_identity(&boot), json!({}))
        .await
        .expect("planner reads before the summary commit");
    call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        json!({
            "message": "long cjk summary",
            "summary": summary,
        }),
    )
    .await
    .expect("planner sets a 200-char summary");
    let wire = call_tool_raw(&boot, TOOL_REPORT_READ, planner_identity(&boot), json!({}))
        .await
        .expect("planner reads the report");
    let line = wire["content"][0]["text"].as_str().expect("text block");
    assert!(
        line.len() < 300,
        "content[0].text must stay one short line for a CJK summary, got {} bytes: {line}",
        line.len()
    );
    let clipped_tail = format!("{}…;", "报".repeat(4));
    assert!(line.contains(&clipped_tail), "{line}");
    assert!(
        line.contains(&format!(" · {} bytes · ", body.len())),
        "{line}"
    );
    assert_eq!(
        wire["structuredContent"]["summary"].as_str(),
        Some(summary.as_str()),
        "the clip is receipt-only; structuredContent carries the whole summary"
    );
}

#[tokio::test]
async fn rev_conflicts_carry_the_current_revisions_in_error_data() {
    use calm_server::mcp_server::tools::track_report_blocks::RPC_REV_CONFLICT;
    let boot = boot().await;
    seed_large_body(&boot).await;
    let read = call_tool(&boot, TOOL_REPORT_READ, planner_identity(&boot), json!({}))
        .await
        .unwrap();
    let id = read["blocks"][0]["id"].as_str().unwrap().to_string();
    let other = upsert_block(
        &boot,
        assistant_identity(&boot),
        json!({"id": id, "kind": "prose", "markdown": "# One\n\nby another writer\n"}),
    )
    .await
    .expect("another session edits the first block");
    let rev = other["rev"].as_u64().unwrap();
    let current = current_doc_rev(&boot).await;

    for (tool, args) in [
        (
            TOOL_REPORT_COMMIT,
            json!({"message": "stale commit", "summary": "stale"}),
        ),
        (
            TOOL_REPORT_WRITE,
            json!({"body": "# stale\n", "message": "stale write"}),
        ),
    ] {
        let err = call_tool(&boot, tool, planner_identity(&boot), args)
            .await
            .expect_err("a stale read must conflict");
        assert_eq!(err.code, RPC_REV_CONFLICT, "{tool}: {err:?}");
        assert!(
            err.message
                .contains(&format!("current doc_rev is {current}")),
            "{tool}: {err:?}"
        );
        assert_eq!(
            err.data.as_ref().and_then(|d| d["docRev"].as_u64()),
            Some(current),
            "{tool}: data.docRev must be the current doc rev: {err:?}"
        );
    }

    let err = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        json!({"message": "stomp", "ops": [
            {"op": "upsert", "id": id, "kind": "prose", "markdown": "# stomp\n"}
        ]}),
    )
    .await
    .expect_err("a stale block read must conflict");
    assert_eq!(err.code, RPC_REV_CONFLICT, "{err:?}");
    assert!(
        err.message.contains(&format!("current rev is {rev}")),
        "{err:?}"
    );
    // Both anchors, so the retry needs no full re-read.
    assert_eq!(
        err.data,
        Some(json!({"docRev": current, "rev": rev})),
        "data must carry the current docRev AND the block's current rev: {err:?}"
    );
    // Nothing was written: the anchors a retry would use are unchanged.
    assert_eq!(current_doc_rev(&boot).await, current);
}
