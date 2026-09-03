//! Issue #229 PR B — `mcp_server::tools::track_report` integration smoke.
//!
//! Same shape as `mcp_track_state.rs`: in-memory `SqlxRepo`, an
//! `EventBus`, a pre-seeded `CardRoleCache`, and an `AppContext`
//! constructed directly so we can drive the three tool handlers
//! (`calm.report.read`, `calm.report.write`, `calm.report.edit`) as
//! plain async fns.
//!
//! Coverage:
//!
//!   1. `report_read` (spec) returns the initial seeded body + summary
//!      + schemaVersion + updated_at.
//!   2. `report_write` (spec) replaces the body wholesale, bumps
//!      `updated_at`, and emits one `card.updated` event.
//!   3. `report_write` keeps the existing summary when omitted; honors
//!      a non-null override when provided.
//!   4. `report_edit` happy path — unique substring replacement.
//!   5. `report_edit` rejects missing `old_string` (-32602).
//!   6. `report_edit` rejects duplicate matches without `replace_all`
//!      (-32602).
//!   7. `report_edit` honors `replace_all=true` on multi-match.
//!   8. `report_edit` short-circuits when `old_string == new_string`
//!      (no write, no event, returns current `updated_at`).
//!   9. Worker calling any of the three is refused at the soft role
//!      gate (-32602 "tool requires role=Spec got=Worker").
//!  10. Spec card on a different track cannot reach this track's report
//!      — the (spec_card_id → track_id → report_card) lookup confines
//!      writes to the caller's own track.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use crate::support::mcp::set_persisted_card_role;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::RepoEventWrite;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_insert_tx, session_mark_track_root_tx};
use calm_server::error::CalmError;
use calm_server::event::{EditAuthor, Event, EventBus, EventScope};
use calm_server::ids::{ActorId, AreaId, CardId, TrackId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::track_report::{
    TOOL_REPORT_EDIT, TOOL_REPORT_READ, TOOL_REPORT_WRITE,
};
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack, TrackLifecycle, TrackPatch};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::session_projection_repo::AgentProvider;
use calm_server::track_report::TrackReportPayload;
use calm_types::worker::{
    LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession, WorkerSessionId,
    WorkerSessionState,
};
use serde_json::{Value, json};

const SPEC_SESSION_ID: &str = "spec-session";
/// #1189 — the assistant's session is deliberately NOT the track root, and
/// it is bound to its own `CardRole::Assistant` card. Both facts are what
/// the S2 recorder criterion actually reads.
pub(crate) const ASSISTANT_SESSION_ID: &str = "assistant-session";
/// #1189 S6 — a **second, independent** assistant conversation on the same
/// track: its own `CardRole::Assistant` card and its own non-root session.
/// §3.3's whole argument ("concurrency is handled by the existing CAS, no
/// locks") is a claim about two of these interleaving, which cannot be
/// expressed with a single session handing itself a stale rev.
pub(crate) const ASSISTANT_B_SESSION_ID: &str = "assistant-b-session";
pub(crate) const WORKER_SESSION_ID: &str = "worker-session";

/// In-memory fixture: one area → one track → one spec card + one
/// track-report card + one worker card. Mirrors the post-`create_track`
/// shape (spec + track-report kernel-owned) plus a worker for the
/// cross-role tests.
pub(crate) struct Boot {
    pub(crate) ctx: Arc<AppContext>,
    pub(crate) registry: Arc<ToolRegistry>,
    pub(crate) repo: Arc<dyn Repo>,
    pub(crate) area_id: AreaId,
    pub(crate) track_id: TrackId,
    pub(crate) spec_card_id: CardId,
    pub(crate) report_card_id: CardId,
    pub(crate) worker_card_id: CardId,
    pub(crate) assistant_card_id: CardId,
    /// #1189 S6 — the second assistant conversation. See
    /// [`ASSISTANT_B_SESSION_ID`].
    pub(crate) assistant_b_card_id: CardId,
    /// #1252 — the same `CardRoleCache` handle that is inside
    /// [`Boot::ctx`]'s `WriteContext` (`CardRoleCache` is an `Arc<DashMap>`
    /// newtype, so this clone shares state). A test that mints a card after
    /// `boot()` has to get it into the cache the role gate reads, and the
    /// production way to do that is `repo.seed_card_role_cache(&cache)` —
    /// the same call `AppState::new` makes at boot (`state.rs:996`).
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

async fn seed_track_root_session(
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

/// A live, card-bound, non-root session row. The recorder gate resolves
/// session → card → {role, track} against these rows, so a test identity
/// without one is denied before any of the S2 behaviour is reached.
///
/// Contract is `Executor`, not `Planner`, and that is load-bearing:
/// `session_mirror.rs:266-270` repoints `tracks.root_session_id` at *any*
/// session whose `contract == Planner` and whose state is an active
/// authority. A Planner-contract session on the assistant card would
/// therefore steal the track root from the spec card the moment it went live
/// — a shape that never occurs in production and that would make these tests
/// a false reference for S3. Matches `frozen_gate_vectors_transport.rs`,
/// which seeds its assistant sessions the same way.
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

/// [`seed_non_root_session`] with the `worker_sessions.provider` column
/// spelled out. #1252 seeds a Claude row so a Claude case's fixture rows
/// match the identity it acts under — but be clear about what that buys:
/// the column is **not** what picks the actor arm.
/// `registry::provider_session_actor` reads `ToolCallIdentity::provider`,
/// which `call_tool` callers set by hand. Verified rather than assumed:
/// seeding this row as `Codex` while leaving the identity `Claude` still
/// leaves
/// `report_write_characterization::mcp_claude_assistant_block_write_is_actored_to_the_claude_session`
/// green, because the session-authority resolution looks the row up by
/// session id and never compares its provider. In production the column
/// does reach the identity, but only through the transport
/// (`crates/calm-server/src/mcp_server/transport.rs:1615-1618`), which
/// `call_tool` bypasses — so no test on this path covers that hop.
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
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
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
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let track = repo
        .track_update(
            track.id.as_str(),
            TrackPatch {
                lifecycle: Some(TrackLifecycle::Planning),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let spec_card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .unwrap();
    // The track-report card row matching what `routes::tracks::create_track`
    // (and migration 0014) mint. These integration tests look up the row
    // by `kind == "track-report"`, not by role/deletable. We pin the role
    // in the cache below to mirror production semantics.
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
    // #1189 — the two assistant conversation cards. Payload is the one
    // production mints (`spec_harness_start_adapter.rs:620`, via
    // `minted_card_shape(HarnessProfile::Assistant)`): a v1 codex payload
    // plus the `harness_profile` marker. It is not decoration — a card
    // without that marker is invisible to the track conversation list
    // (`track_conversations.rs:327`) and cannot receive a message
    // (`plain_chat::card_is_track_assistant`, `cards.rs:150`), so a fixture
    // assistant card without it is not the thing production makes.
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
    set_persisted_card_role(repo.as_ref(), spec_card.id.as_str(), CardRole::Spec).await;
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
    seed_track_root_session(repo.as_ref(), &track.id, &spec_card.id, SPEC_SESSION_ID).await;
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
    card_role_cache.insert(spec_card.id.clone(), CardRole::Spec, track.id.clone());
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
        spec_card_id: spec_card.id,
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
    handler(boot.ctx.clone(), identity, args).await
}

async fn current_doc_rev(boot: &Boot) -> u64 {
    calm_server::track_report_read::load_report_read_snapshot(
        boot.repo.as_ref(),
        boot.report_card_id.as_str(),
    )
    .await
    .expect("read current document revision")
    .doc_rev
}

pub(crate) fn spec_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.spec_card_id.as_str().to_string(),
        role: CardRole::Spec,
        provider: AgentProvider::Codex,
        session_id: SPEC_SESSION_ID.to_string(),
        track_id: Some(boot.track_id.as_str().to_string()),
        area_id: boot.area_id.as_str().to_string(),
        thread_id: "spec-thread".to_string(),
    }
}

/// #1189 — an `CardRole::Assistant` caller on this track: its own card, its
/// own non-root session.
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

/// #1189 S6 — the *other* assistant conversation on this same track: a
/// distinct `CardRole::Assistant` card with the production `harness_profile`
/// marker, a distinct live non-root `worker_sessions` row bound to it, same
/// track and area.
///
/// **What this is not**: it is not minted by the production route. A real
/// second conversation is born inside the harness-start operation
/// (`spec_harness_start_adapter`), which also marks the card kernel-owned
/// (`deletable = false`) and issues per-card / per-session MCP tokens that
/// the transport then binds to the identity it hands a tool. Here the rows
/// are inserted directly and the [`ToolCallIdentity`] is constructed by the
/// test, so the token issuance and the transport's token → identity binding
/// are out of frame — nothing in these tests could notice if they broke.
///
/// That is sound for what these tests claim, and only for that. The write
/// path they exercise re-derives everything it authorizes on from the
/// database: the recorder gate looks the `session_id` up in the real
/// `worker_sessions` table, follows it to the real `cards.role`, and checks
/// the real `cards.track_id`. A hand-made identity that did not correspond to
/// those rows would be refused before reaching any CAS. So the CAS
/// conclusions in `mcp_report_concurrent_sessions.rs` hold; a claim about
/// how conversations are *created* would not, and is not made here.
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

/// Subscribe to the bus and collect `n` envelopes — small helper so
/// the write/edit tests can assert on the emitted `card.updated`.
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

// ---------------------------------------------------------------------------
// calm.report.read
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_returns_initial_seeded_body() {
    let boot = boot().await;
    let out = call_tool(&boot, TOOL_REPORT_READ, spec_identity(&boot), json!({}))
        .await
        .expect("spec can read the report");
    assert_eq!(
        out.get("body").and_then(Value::as_str),
        Some(TrackReportPayload::initial().body.as_str())
    );
    assert_eq!(out.get("summary").and_then(Value::as_str), Some(""));
    assert_eq!(out.get("schemaVersion").and_then(Value::as_u64), Some(3));
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
    assert!(err.message.contains("Spec"), "msg = {err:?}");
}

// ---------------------------------------------------------------------------
// calm.report.write
// ---------------------------------------------------------------------------

#[tokio::test]
async fn whole_document_write_requires_if_doc_rev_and_rejects_stale_spec_writer() {
    let boot = boot().await;
    let missing = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({"body": "# A\n", "message": "missing revision"}),
    )
    .await
    .unwrap_err();
    assert_eq!(missing.code, -32602);

    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({"body": "# First\n", "message": "first writer", "if_doc_rev": 0}),
    )
    .await
    .unwrap();
    let conflict = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({"body": "# Stale\n", "message": "second writer", "if_doc_rev": 0}),
    )
    .await
    .unwrap_err();
    assert_eq!(conflict.code, -32001);
    assert!(conflict.message.contains("current doc_rev is 1"));
    assert!(conflict.message.contains("expected if_doc_rev 0"));
    assert!(conflict.message.contains("re-read"));
    let read = call_tool(&boot, TOOL_REPORT_READ, spec_identity(&boot), json!({}))
        .await
        .unwrap();
    assert_eq!(read["body"], "# First\n", "stale writer must not win");
}

#[tokio::test]
async fn write_replaces_body_and_emits_card_updated() {
    let boot = boot().await;
    let events = boot.ctx.events.clone();
    let report_id = boot.report_card_id.clone();
    let track_id = boot.track_id.clone();
    // PR2 of #247 — every persist_report call now emits TWO envelopes:
    //   1. Event::CardUpdated (generic "row changed" signal — existing PR1 behavior)
    //   2. Event::TrackReportEdited (structured edit-log entry — new in PR2)
    // Subscribe early and collect both so the test can assert order + payload.
    let sub = tokio::spawn(async move { collect_n(&events, 2).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let out = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "# Goal\n\nrefactored everything\n",
            "summary": "done refactoring",
            "message": "rewrite report",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
    )
    .await
    .expect("spec writes successfully");
    let new_updated_at = out
        .get("updated_at")
        .and_then(Value::as_i64)
        .expect("updated_at i64");
    assert_eq!(out.get("docRev").and_then(Value::as_u64), Some(1));

    // Bus saw exactly two envelopes: CardUpdated first (preserves
    // pre-PR2 broadcast order so the generic "re-fetch" signal lands
    // before the structured edit-log entry), then TrackReportEdited.
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
            assert_eq!(payload.schema_version, 3);
            assert_eq!(payload.doc_rev, 1);
            assert_eq!(c.updated_at, new_updated_at);
        }
        other => panic!("expected CardUpdated first, got {other:?}"),
    }
    assert!(matches!(envs[0].scope, EventScope::Card { .. }));

    // Second envelope: structured TrackReportEdited.
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
            // Issue #247 PR3 — the MCP `report.write` / `report.edit`
            // wrapper now passes `EditAuthor::Spec` explicitly (was
            // hard-coded in PR2). REST callers go through the same
            // shared `track_report::persist_report` but pass
            // `EditAuthor::User` — see `tests/rest_track_report.rs` for
            // the User-author regression. Spec attribution stays the
            // contract for every spec-MCP write.
            assert_eq!(*author, EditAuthor::Spec, "MCP path tags Spec");
            assert_eq!(agent_message.as_deref(), Some("rewrite report"));
            // edit_id must be a non-empty UUID-shaped string. Don't pin
            // the exact value — it's a fresh UUID per call.
            assert!(!edit_id.is_empty(), "edit_id must be a non-empty UUID");
            // UUID v4 string is 36 chars (8-4-4-4-12 with hyphens).
            assert_eq!(
                edit_id.len(),
                36,
                "edit_id should be a UUID v4 string; got {edit_id:?}",
            );
            // Pre-write state: the seed body + empty summary that
            // `boot()` minted via `TrackReportPayload::initial()`.
            assert_eq!(
                summary_before, "",
                "pre-write summary is the empty initial value",
            );
            assert_eq!(
                body_before,
                &TrackReportPayload::initial().body,
                "pre-write body is the initial seed body",
            );
            // Post-write state: matches what was passed to report.write.
            assert_eq!(summary_after, "done refactoring");
            assert_eq!(body_after, "# Goal\n\nrefactored everything\n");
        }
        other => panic!("expected TrackReportEdited second, got {other:?}"),
    }
    // Same card scope as the CardUpdated envelope, and the scope row
    // must also populate `scope_track` + `scope_card` so the dispatcher's
    // push filter can subscribe to the track's edit log without scanning
    // the firehose.
    match &envs[1].scope {
        EventScope::Card { card, track, .. } => {
            assert_eq!(card, &report_id, "scope_card persisted on the events row");
            assert_eq!(track, &track_id, "scope_track persisted on the events row");
        }
        other => panic!("expected Card-scoped envelope, got {other:?}"),
    }

    // DB also has the new shape.
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
async fn write_requires_non_empty_message() {
    let boot = boot().await;

    let err = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({ "body": "missing message\n" }),
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
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({ "body": "empty message\n", "message": "   ", "if_doc_rev": current_doc_rev(&boot).await }),
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
async fn write_without_lifecycle_keeps_track_state_and_records_agent_message() {
    let boot = boot().await;
    let mut rx = boot.ctx.events.subscribe();

    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "no lifecycle body\n",
            "message": "write without lifecycle",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
    )
    .await
    .expect("write succeeds");

    let card_env = recv_env(&mut rx).await;
    assert!(matches!(card_env.event, Event::CardUpdated(_)));
    let report_env = recv_env(&mut rx).await;
    match &report_env.event {
        Event::TrackReportEdited { agent_message, .. } => {
            assert_eq!(agent_message.as_deref(), Some("write without lifecycle"))
        }
        other => panic!("expected TrackReportEdited, got {other:?}"),
    }
    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(track.lifecycle, TrackLifecycle::Planning);
    let no_more = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
    assert!(no_more.is_err(), "unexpected lifecycle event: {no_more:?}");
}

#[tokio::test]
async fn write_from_draft_auto_promotes_with_lifecycle_changed_event() {
    let boot = boot().await;
    boot.repo
        .track_update(
            boot.track_id.as_str(),
            TrackPatch {
                lifecycle: Some(TrackLifecycle::Draft),
                ..Default::default()
            },
        )
        .await
        .expect("set draft lifecycle");
    let mut rx = boot.ctx.events.subscribe();

    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "auto-promote body\n",
            "message": "write from draft",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
    )
    .await
    .expect("write succeeds");

    let changed_env = recv_env(&mut rx).await;
    assert!(matches!(changed_env.actor, ActorId::Kernel));
    match changed_env.event {
        Event::TrackLifecycleChanged {
            id,
            from,
            to,
            agent_message,
            ..
        } => {
            assert_eq!(id, boot.track_id);
            assert_eq!(from, TrackLifecycle::Draft);
            assert_eq!(to, TrackLifecycle::Planning);
            assert_eq!(agent_message.as_deref(), Some("[auto] first spec write"));
        }
        other => panic!("expected auto TrackLifecycleChanged first, got {other:?}"),
    }

    let updated_env = recv_env(&mut rx).await;
    assert!(matches!(updated_env.actor, ActorId::Kernel));
    match updated_env.event {
        Event::TrackUpdated(payload) => {
            assert_eq!(payload.id, boot.track_id);
            assert_eq!(payload.lifecycle, TrackLifecycle::Planning);
            assert_eq!(
                payload.agent_message.as_deref(),
                Some("[auto] first spec write")
            );
        }
        other => panic!("expected auto TrackUpdated second, got {other:?}"),
    }
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
            assert_eq!(agent_message.as_deref(), Some("write from draft"));
            assert_eq!(body_after, "auto-promote body\n");
        }
        other => panic!("expected TrackReportEdited fourth, got {other:?}"),
    }

    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(track.lifecycle, TrackLifecycle::Planning);
    let no_more = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
    assert!(no_more.is_err(), "unexpected extra event: {no_more:?}");
}

#[tokio::test]
async fn write_lifecycle_legal_emits_track_updated_and_report_events() {
    let boot = boot().await;
    let mut rx = boot.ctx.events.subscribe();

    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "dispatching body\n",
            "message": "report moves dispatching",
            "if_doc_rev": current_doc_rev(&boot).await,
            "lifecycle": "dispatching"
        }),
    )
    .await
    .expect("write with lifecycle succeeds");

    let changed_env = recv_env(&mut rx).await;
    match &changed_env.event {
        Event::TrackLifecycleChanged {
            id,
            from,
            to,
            agent_message,
            ..
        } => {
            assert_eq!(id, &boot.track_id);
            assert_eq!(*from, TrackLifecycle::Planning);
            assert_eq!(*to, TrackLifecycle::Dispatching);
            assert_eq!(agent_message.as_deref(), Some("report moves dispatching"));
        }
        other => panic!("expected TrackLifecycleChanged first, got {other:?}"),
    }
    let updated_env = recv_env(&mut rx).await;
    match &updated_env.event {
        Event::TrackUpdated(payload) => {
            assert_eq!(payload.id, boot.track_id);
            assert_eq!(payload.lifecycle, TrackLifecycle::Dispatching);
            assert_eq!(
                payload.agent_message.as_deref(),
                Some("report moves dispatching")
            );
        }
        other => panic!("expected TrackUpdated second, got {other:?}"),
    }
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
            assert_eq!(agent_message.as_deref(), Some("report moves dispatching"));
            assert_eq!(body_after, "dispatching body\n");
        }
        other => panic!("expected TrackReportEdited fourth, got {other:?}"),
    }
    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(track.lifecycle, TrackLifecycle::Dispatching);
}

#[tokio::test]
async fn write_lifecycle_illegal_rolls_back_report_and_events() {
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

    let err = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "should rollback\n",
            "message": "illegal report lifecycle",
            "if_doc_rev": current_doc_rev(&boot).await,
            "lifecycle": "done"
        }),
    )
    .await
    .expect_err("planning -> done is illegal");
    assert_eq!(err.code, -32403);

    let after_track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after_track.lifecycle, before_track.lifecycle);
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
        "illegal transition emitted event: {no_event:?}"
    );
}

// ---------------------------------------------------------------------------
// PR2 of #247 — Event::TrackReportEdited coverage.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn edit_emits_track_report_edited_alongside_card_updated() {
    let boot = boot().await;
    // Seed a known body so the before/after diff is predictable.
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "before XYZ after\n",
            "summary": "before-summary",
            "message": "seed report",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
    )
    .await
    .expect("seed write");

    // Now subscribe before issuing the edit — we expect TWO envelopes
    // (CardUpdated + TrackReportEdited) from a single `report.edit`
    // call, identical to the `report.write` path.
    let events = boot.ctx.events.clone();
    let report_id = boot.report_card_id.clone();
    let track_id = boot.track_id.clone();
    let sub = tokio::spawn(async move { collect_n(&events, 2).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        spec_identity(&boot),
        json!({
            "old_string": "XYZ",
            "new_string": "ABC",
            "message": "edit report",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
    )
    .await
    .expect("edit succeeds");

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
            assert_eq!(*author, EditAuthor::Spec);
            assert_eq!(agent_message.as_deref(), Some("edit report"));
            assert_eq!(edit_id.len(), 36, "edit_id is a UUID v4 string");
            // Summary unchanged by report.edit — both before and after
            // are the seeded summary.
            assert_eq!(summary_before, "before-summary");
            assert_eq!(summary_after, "before-summary");
            assert_eq!(body_before, "before XYZ after\n");
            assert_eq!(body_after, "before ABC after\n");
        }
        other => panic!("expected TrackReportEdited, got {other:?}"),
    }
}

#[tokio::test]
async fn write_with_unchanged_content_still_emits_track_report_edited() {
    // Invariant: every persist_report call → one CardUpdated + one
    // TrackReportEdited. Re-asserting the same body twice produces a
    // second TrackReportEdited with `body_before == body_after`. PR4's
    // UI can filter no-op entries from the timeline if it wants.
    let boot = boot().await;
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "stable body\n",
            "summary": "stable summary",
            "message": "first stable report",
            "if_doc_rev": current_doc_rev(&boot).await
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

    // Second write with identical body + summary.
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "stable body\n",
            "summary": "stable summary",
            "message": "second stable report",
            "if_doc_rev": current_doc_rev(&boot).await
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
    // The `TrackReportEdited` row must land in the `events` table with
    // `scope_track = track_id` and `scope_card = card_id` so the
    // dispatcher's push filter can subscribe to a single track's edit log
    // without scanning the firehose. Query the table directly through
    // the replay path so
    // we're testing what's persisted, not just what's broadcast.
    let boot = boot().await;
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "scoped body\n",
            "summary": "scoped summary",
            "message": "scoped report",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
    )
    .await
    .expect("write succeeds");

    // Replay every event through the same path the WS handler uses
    // (`events_since`). The tuple shape `(id, version, scope, event)`
    // is reconstructed from the `events.scope_*` columns — so a
    // round-trip back through this path is the strongest assertion
    // available that the row was persisted with the correct scope
    // columns. Filter to the TrackReportEdited rows for the report
    // card and assert the reconstructed scope matches.
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
    // Payload round-trips with the spec author + the seed body before /
    // new body after.
    match ev {
        Event::TrackReportEdited {
            author,
            author_plugin_id: _,
            body_before,
            body_after,
            summary_after,
            ..
        } => {
            assert_eq!(*author, EditAuthor::Spec);
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
async fn write_preserves_summary_when_omitted() {
    let boot = boot().await;
    // First write sets a known summary.
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "a",
            "summary": "preserved",
            "message": "set summary",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
    )
    .await
    .unwrap();
    // Second write omits summary; it should keep "preserved".
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({ "body": "b", "message": "preserve summary", "if_doc_rev": current_doc_rev(&boot).await }),
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
async fn write_refuses_worker() {
    let boot = boot().await;
    let err = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        worker_identity(&boot),
        json!({ "body": "evil", "message": "worker write", "if_doc_rev": current_doc_rev(&boot).await }),
    )
    .await
    .expect_err("worker must be denied");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
}

#[tokio::test]
async fn write_rejects_missing_body() {
    let boot = boot().await;
    let err = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({ "summary": "no body", "message": "missing body", "if_doc_rev": current_doc_rev(&boot).await }),
    )
    .await
    .expect_err("missing body must be rejected");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("body"), "msg = {err:?}");
}

// ---------------------------------------------------------------------------
// calm.report.edit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn edit_unique_substring_replacement_happy_path() {
    let boot = boot().await;
    // Seed a body with a known unique substring.
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "# Goal\n\nuntouched marker XYZ here\n",
            "message": "seed edit body",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
    )
    .await
    .unwrap();
    // Now edit it.
    let out = call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        spec_identity(&boot),
        json!({
            "old_string": "XYZ",
            "new_string": "ABC",
            "message": "replace marker",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
    )
    .await
    .expect("happy edit");
    assert!(out.get("updated_at").and_then(Value::as_i64).is_some());

    let card = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let payload: TrackReportPayload = serde_json::from_value(card.payload).unwrap();
    assert_eq!(payload.body, "# Goal\n\nuntouched marker ABC here\n");
}

#[tokio::test]
async fn edit_requires_non_empty_message() {
    let boot = boot().await;

    let err = call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        spec_identity(&boot),
        json!({ "old_string": "Goal", "new_string": "Plan" }),
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
        TOOL_REPORT_EDIT,
        spec_identity(&boot),
        json!({
            "old_string": "Goal",
            "new_string": "Plan",
            "message": "\n\t ",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
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
async fn edit_without_lifecycle_keeps_track_state_and_records_agent_message() {
    let boot = boot().await;
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "before XYZ after\n",
            "message": "seed edit no lifecycle",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
    )
    .await
    .expect("seed body");
    let mut rx = boot.ctx.events.subscribe();

    call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        spec_identity(&boot),
        json!({
            "old_string": "XYZ",
            "new_string": "ABC",
            "message": "edit without lifecycle",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
    )
    .await
    .expect("edit succeeds");

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
    assert_eq!(track.lifecycle, TrackLifecycle::Planning);
    let no_more = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
    assert!(no_more.is_err(), "unexpected lifecycle event: {no_more:?}");
}

#[tokio::test]
async fn edit_lifecycle_legal_emits_track_updated_and_report_events() {
    let boot = boot().await;
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "before XYZ after\n",
            "message": "seed edit lifecycle",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
    )
    .await
    .expect("seed body");
    let mut rx = boot.ctx.events.subscribe();

    call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        spec_identity(&boot),
        json!({
            "old_string": "XYZ",
            "new_string": "ABC",
            "message": "edit moves dispatching",
            "if_doc_rev": current_doc_rev(&boot).await,
            "lifecycle": "dispatching"
        }),
    )
    .await
    .expect("edit with lifecycle succeeds");

    match recv_env(&mut rx).await.event {
        Event::TrackLifecycleChanged {
            id,
            from,
            to,
            agent_message,
            ..
        } => {
            assert_eq!(id, boot.track_id);
            assert_eq!(from, TrackLifecycle::Planning);
            assert_eq!(to, TrackLifecycle::Dispatching);
            assert_eq!(agent_message.as_deref(), Some("edit moves dispatching"));
        }
        other => panic!("expected TrackLifecycleChanged first, got {other:?}"),
    }
    match recv_env(&mut rx).await.event {
        Event::TrackUpdated(payload) => {
            assert_eq!(payload.id, boot.track_id);
            assert_eq!(payload.lifecycle, TrackLifecycle::Dispatching);
            assert_eq!(
                payload.agent_message.as_deref(),
                Some("edit moves dispatching")
            );
        }
        other => panic!("expected TrackUpdated second, got {other:?}"),
    }
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
            assert_eq!(agent_message.as_deref(), Some("edit moves dispatching"));
            assert_eq!(body_after, "before ABC after\n");
        }
        other => panic!("expected TrackReportEdited fourth, got {other:?}"),
    }
    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(track.lifecycle, TrackLifecycle::Dispatching);
}

#[tokio::test]
async fn edit_lifecycle_illegal_rolls_back_report_and_events() {
    let boot = boot().await;
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "before XYZ after\n",
            "message": "seed illegal edit",
            "if_doc_rev": current_doc_rev(&boot).await
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

    let err = call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        spec_identity(&boot),
        json!({
            "old_string": "XYZ",
            "new_string": "ABC",
            "message": "illegal edit lifecycle",
            "if_doc_rev": current_doc_rev(&boot).await,
            "lifecycle": "done"
        }),
    )
    .await
    .expect_err("planning -> done is illegal");
    assert_eq!(err.code, -32403);

    let after_track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after_track.lifecycle, before_track.lifecycle);
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
        "illegal transition emitted event: {no_event:?}"
    );
}

#[tokio::test]
async fn edit_rejects_old_string_not_found() {
    let boot = boot().await;
    let err = call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        spec_identity(&boot),
        json!({
            "old_string": "nowhere-in-body",
            "new_string": "x",
            "message": "missing old string",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
    )
    .await
    .expect_err("missing old_string must error");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("not found"), "msg = {err:?}");
}

#[tokio::test]
async fn edit_rejects_duplicate_without_replace_all() {
    let boot = boot().await;
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "TODO foo\nTODO bar\n",
            "message": "seed duplicates",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
    )
    .await
    .unwrap();
    let err = call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        spec_identity(&boot),
        json!({
            "old_string": "TODO",
            "new_string": "DONE",
            "message": "duplicate replace",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
    )
    .await
    .expect_err("duplicate without replace_all must error");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("not unique"), "msg = {err:?}");
    assert!(err.message.contains("replace_all"), "msg = {err:?}");
}

#[tokio::test]
async fn edit_replace_all_on_duplicates() {
    let boot = boot().await;
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "TODO foo\nTODO bar\nTODO baz\n",
            "message": "seed replace all",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
    )
    .await
    .unwrap();
    call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        spec_identity(&boot),
        json!({
            "old_string": "TODO",
            "new_string": "DONE",
            "replace_all": true,
            "message": "replace all",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
    )
    .await
    .expect("replace_all=true succeeds");

    let card = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let payload: TrackReportPayload = serde_json::from_value(card.payload).unwrap();
    assert_eq!(payload.body, "DONE foo\nDONE bar\nDONE baz\n");
}

#[tokio::test]
async fn edit_with_identical_old_and_new_still_emits_both_events() {
    // Issue #247 PR2 review fix: `report.edit` used to short-circuit
    // when `old_string == new_string` (return early, no write, no
    // event). That broke symmetry with `report.write` — a
    // content-equal `report.write` still emitted both `CardUpdated`
    // and `TrackReportEdited` (see
    // `write_with_unchanged_content_still_emits_track_report_edited`),
    // while a `report.edit` with equal strings emitted nothing.
    // After the fix every persist path emits exactly the same
    // two-event pair, with `body_before == body_after` and
    // `summary_before == summary_after` for the equal-strings case.
    let boot = boot().await;
    // Seed a known body. The substring "stable" must exist for the
    // post-fix flow to find it (the old `old == new` short-circuit
    // ran *before* the not-found check; now both checks run).
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "stable\n",
            "summary": "stable-summary",
            "message": "seed equal edit",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
    )
    .await
    .unwrap();
    let before = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let before_ts = before.updated_at;
    let report_id = boot.report_card_id.clone();
    let track_id = boot.track_id.clone();

    // Subscribe — we now expect TWO envelopes from the equal-strings
    // edit, identical to the `report.write` path.
    let events = boot.ctx.events.clone();
    let sub = tokio::spawn(async move { collect_n(&events, 2).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let out = call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        spec_identity(&boot),
        json!({
            "old_string": "stable",
            "new_string": "stable",
            "message": "equal edit",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
    )
    .await
    .expect("equal-strings edit succeeds (content-equal write)");
    let new_ts = out
        .get("updated_at")
        .and_then(Value::as_i64)
        .expect("updated_at i64");
    assert!(
        new_ts >= before_ts,
        "content-equal edit bumps (or keeps) updated_at; before={before_ts} after={new_ts}",
    );

    // Bus must see exactly two envelopes: CardUpdated then
    // TrackReportEdited, same invariant as `report.write`.
    let envs = sub.await.expect("collector ok");
    assert_eq!(
        envs.len(),
        2,
        "equal-strings edit emits both events (symmetry with report.write); got {envs:?}",
    );
    assert!(
        matches!(envs[0].event, Event::CardUpdated(_)),
        "CardUpdated first (preserves pre-PR2 broadcast order)",
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
            assert_eq!(w, &track_id, "track_id matches");
            assert_eq!(c, &report_id, "card_id matches");
            assert_eq!(*author, EditAuthor::Spec);
            assert_eq!(agent_message.as_deref(), Some("equal edit"));
            assert_eq!(edit_id.len(), 36, "edit_id is a UUID v4 string");
            // The defining assertion: equal-strings replacement is
            // the identity map, so before == after on both fields.
            assert_eq!(
                body_before, body_after,
                "equal-strings edit: body_before == body_after",
            );
            assert_eq!(
                summary_before, summary_after,
                "equal-strings edit: summary_before == summary_after",
            );
            assert_eq!(body_before, "stable\n");
            assert_eq!(summary_before, "stable-summary");
        }
        other => panic!("expected TrackReportEdited, got {other:?}"),
    }

    // Row's payload is unchanged byte-for-byte (it's the same body).
    let after = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let payload: TrackReportPayload = serde_json::from_value(after.payload).unwrap();
    assert_eq!(payload.body, "stable\n");
    assert_eq!(payload.summary, "stable-summary");
}

#[tokio::test]
async fn edit_refuses_worker() {
    let boot = boot().await;
    let err = call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        worker_identity(&boot),
        json!({
            "old_string": "Goal",
            "new_string": "Pwn",
            "message": "worker edit",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
    )
    .await
    .expect_err("worker must be denied");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
}

// ---------------------------------------------------------------------------
// Cross-track isolation: a spec card on track A cannot reach track B's report.
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(deprecated)]
async fn spec_from_different_track_cannot_reach_this_track_report() {
    let boot = boot().await;
    // Mint a second track + a second spec card, and use that spec
    // identity to call `report.write`. The tool resolves the report
    // through (spec_card_id → spec_card.track_id → track's report card),
    // so the write lands on track 2's report — *not* track 1's. We
    // confirm track 1's body is untouched.

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
    let spec2 = boot
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
    set_persisted_card_role(boot.repo.as_ref(), spec2.id.as_str(), CardRole::Spec).await;
    seed_track_root_session(boot.repo.as_ref(), &track2.id, &spec2.id, "spec2-session").await;
    boot.ctx
        .write
        .role_cache()
        .insert(spec2.id.clone(), CardRole::Spec, track2.id.clone());

    // Call from spec2's identity.
    let spec2_identity = ToolCallIdentity {
        card_id: spec2.id.as_str().to_string(),
        role: CardRole::Spec,
        provider: AgentProvider::Codex,
        session_id: "spec2-session".to_string(),
        track_id: Some(track2.id.as_str().to_string()),
        area_id: area2.id.as_str().to_string(),
        thread_id: "spec2-thread".to_string(),
    };
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec2_identity,
        json!({
            "body": "track 2 only\n",
            "summary": "track 2",
            "message": "track 2 report",
            "if_doc_rev": current_doc_rev(&boot).await
        }),
    )
    .await
    .expect("spec2 writes its own track's report");

    // Track 1's report is untouched.
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

    // Track 2's report has the new body.
    let card2 = boot
        .repo
        .card_get(report2.id.as_str())
        .await
        .unwrap()
        .unwrap();
    let payload2: TrackReportPayload = serde_json::from_value(card2.payload).unwrap();
    assert_eq!(payload2.body, "track 2 only\n");
    assert_eq!(payload2.summary, "track 2");

    // Use track_id to silence unused-variable lints — referenced for
    // potential future per-track-id assertions.
    let _ = boot.track_id.clone();
}
