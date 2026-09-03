//! Issue #1211 S3 — `calm.track.rename`, the planner agent's naming write.
//!
//! Same shape as `mcp_track_state`: an in-memory repo + a directly-constructed
//! `AppContext`, tools driven through the registry the way the transport
//! drives them. What is under test here is the *guard*, not the prompt — the
//! planner prompt tells the agent to name the track, and the golden in
//! `plugin_host::manifest` pins that text, but a prompt is advice. These tests
//! pin the refusals a misbehaving (or merely confused) agent runs into.
//!
//! Covered:
//!
//!   1. Happy path — an unnamed track gets its name, the row changes, and the
//!      `TrackUpdated` event is attributed to the **planner session**, not to the
//!      user.
//!   2. Name-once — a track that already has a title refuses with
//!      `already_named` and does not change the row or emit anything.
//!   3. Role gate — Worker / Assistant / ReportCard identities are refused by
//!      `require_role` before any write.
//!   4. Template tracks refuse (`template_track`).
//!   5. Area-chat tracks refuse (`chat_track`).
//!   6. Whitespace-only / missing titles are argument errors, and a title is
//!      stored trimmed.

use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::RepoEventWrite;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, begin_immediate_tx, session_insert_tx, session_mark_track_root_tx, track_create_tx,
};
use calm_server::error::CalmError;
use calm_server::event::{Event, EventBus};
use calm_server::ids::{ActorId, AreaId, CardId, TrackId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::track_rename::TOOL_TRACK_RENAME;
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{CardRole, NewArea, NewCard, NewOverlay, NewTrack};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::session_projection_repo::AgentProvider;
use calm_server::validation::{
    OVERLAY_TEMPLATE_ENTITY_KIND, OVERLAY_TEMPLATE_KIND, OVERLAY_TEMPLATE_PLUGIN_ID,
    template_overlay_payload,
};
use calm_types::worker::{
    LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession, WorkerSessionId,
    WorkerSessionState,
};
use serde_json::{Value, json};

const PLANNER_SESSION_ID: &str = "planner-session-rename";

struct Boot {
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    repo: Arc<dyn Repo>,
    sqlx_repo: Arc<SqlxRepo>,
    area_id: AreaId,
    track_id: TrackId,
    planner_card_id: CardId,
    other_card_id: CardId,
}

impl Boot {
    fn pool(&self) -> &sqlx::SqlitePool {
        self.sqlx_repo.pool()
    }
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

/// `title` is a parameter because the whole contract turns on whether the track
/// is already named; `purpose` is one because the area-chat track is created
/// through `track_create_tx`'s purpose argument in production
/// (`routes::tracks`), not by patching the column afterwards.
async fn boot_with(title: &str, purpose: Option<&'static str>) -> Boot {
    let sqlx_repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let area = repo
        .area_create(NewArea {
            name: "mcp-track-rename".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    let new_track = NewTrack {
        template_input: None,
        area_id: area.id.clone(),
        title: title.into(),
        sort: None,
        cwd: String::new(),
        template_id: None,
        plugin_scope: None,
        attach_folder: false,
        theme: calm_server::routes::theme::RequestTheme::default_dark(),
    };
    let track = {
        let cache = track_area_cache.clone();
        let mut tx = begin_immediate_tx(sqlx_repo.pool()).await.unwrap();
        let track = track_create_tx(
            &mut tx,
            new_track,
            purpose,
            &calm_server::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
            None,
            &cache,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        track
    };
    let planner_card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "planner".into(),
            sort: None,
            payload: serde_json::Value::Null,
        })
        .await
        .unwrap();
    let other_card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: serde_json::Value::Null,
        })
        .await
        .unwrap();
    seed_track_root_session(
        repo.as_ref(),
        &track.id,
        &planner_card.id,
        PLANNER_SESSION_ID,
    )
    .await;

    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    card_role_cache.insert(planner_card.id.clone(), CardRole::Planner, track.id.clone());
    crate::support::mcp::set_persisted_card_role(
        repo.as_ref(),
        planner_card.id.as_str(),
        CardRole::Planner,
    )
    .await;
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();
    let ctx = Arc::new(AppContext {
        repo: route_repo,
        track_vcs: repo
            .sqlite_pool()
            .map(calm_truth::track_vcs_repo::SqlxTrackVcsRepo::shared),
        events,
        write: calm_server::state::WriteContext::new(card_role_cache, track_area_cache),
        daemon_token_hash: None,
        gate_logs_dir: std::env::temp_dir().join("neige-test-gate-logs"),
        plugin_host: Arc::new(tokio::sync::OnceCell::new()),
        operation_runtime: Arc::new(tokio::sync::OnceCell::new()),
    });

    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);

    Boot {
        ctx,
        registry: Arc::new(registry),
        repo,
        sqlx_repo,
        area_id: area.id,
        track_id: track.id,
        planner_card_id: planner_card.id,
        other_card_id: other_card.id,
    }
}

async fn boot_unnamed() -> Boot {
    boot_with("", None).await
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

fn identity_for(boot: &Boot, card_id: &CardId, role: CardRole, session: &str) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: card_id.as_str().to_string(),
        role,
        provider: AgentProvider::Codex,
        session_id: session.to_string(),
        track_id: Some(boot.track_id.as_str().to_string()),
        area_id: boot.area_id.as_str().to_string(),
        thread_id: format!("{session}-thread"),
    }
}

fn planner_identity(boot: &Boot) -> ToolCallIdentity {
    identity_for(
        boot,
        &boot.planner_card_id,
        CardRole::Planner,
        PLANNER_SESSION_ID,
    )
}

async fn track_title(boot: &Boot) -> String {
    boot.repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .expect("track row")
        .title
}

// ---------------------------------------------------------------------------
// Happy path
// ---------------------------------------------------------------------------

/// The write lands on the row AND the audit row says the agent did it.
///
/// The actor assertion is the load-bearing half. A rename implemented through
/// the user-facing PATCH would work perfectly and still be wrong: "who named
/// this track" is exactly the question a user asks when a name surprises them,
/// and an `ActorId::User` row answers it with a lie.
#[tokio::test]
async fn planner_names_an_unnamed_track_and_the_event_is_attributed_to_the_planner_session() {
    let boot = boot_unnamed().await;
    let mut rx = boot.ctx.events.subscribe();

    let out = call_tool(
        &boot,
        TOOL_TRACK_RENAME,
        planner_identity(&boot),
        json!({ "title": "  drop the title→goal seeding  ", "message": "named from the conversation" }),
    )
    .await
    .expect("planner may name an unnamed track");

    assert_eq!(out.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        out.get("title").and_then(Value::as_str),
        Some("drop the title→goal seeding"),
        "the stored title is trimmed"
    );
    assert_eq!(track_title(&boot).await, "drop the title→goal seeding");

    let envelope = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers")
        .expect("bus open");
    match &envelope.actor {
        ActorId::AiPlannerSession(session) => assert_eq!(session.as_str(), PLANNER_SESSION_ID),
        other => panic!("rename must be attributed to the planner session, got {other:?}"),
    }
    assert!(
        !matches!(envelope.actor, ActorId::User),
        "a rename must never be attributed to the user"
    );
    match envelope.event {
        Event::TrackUpdated(payload) => {
            assert_eq!(payload.track.title, "drop the title→goal seeding");
            assert_eq!(
                payload.agent_message.as_deref(),
                Some("named from the conversation")
            );
        }
        other => panic!("expected TrackUpdated, got {other:?}"),
    }

    // And the persisted `events.actor` column carries the same attribution,
    // not just the in-process broadcast. The column is the durable half of
    // the claim; the bus envelope above is derived from the same value.
    let actors: Vec<String> =
        sqlx::query_scalar("SELECT actor FROM events WHERE kind = 'track.updated' ORDER BY id")
            .fetch_all(boot.pool())
            .await
            .unwrap();
    let stored: Vec<ActorId> = actors
        .iter()
        .map(|a| serde_json::from_str(a).expect("actor column is a typed ActorId"))
        .collect();
    assert!(
        stored
            .iter()
            .any(|a| matches!(a, ActorId::AiPlannerSession(s) if s.as_str() == PLANNER_SESSION_ID)),
        "persisted event row must carry the planner session actor: {actors:?}"
    );
    assert!(
        !stored.iter().any(|a| matches!(a, ActorId::User)),
        "no rename row may be attributed to the user: {actors:?}"
    );
}

// ---------------------------------------------------------------------------
// Name-once
// ---------------------------------------------------------------------------

/// The core guard. A named track refuses structurally — not a panic, not a
/// 500, and above all not a silent overwrite of a name the user chose.
#[tokio::test]
async fn already_named_track_refuses_structurally_and_changes_nothing() {
    let boot = boot_with("the user's own name", None).await;
    let mut rx = boot.ctx.events.subscribe();

    let out = call_tool(
        &boot,
        TOOL_TRACK_RENAME,
        planner_identity(&boot),
        json!({ "title": "the agent's idea" }),
    )
    .await
    .expect("a refusal is a value, not an RPC error");

    assert_eq!(
        out,
        json!({ "ok": false, "refused": "already_named", "title": "the user's own name" }),
    );
    assert_eq!(track_title(&boot).await, "the user's own name");
    let no_event = tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(no_event.is_err(), "refusal emitted an event: {no_event:?}");
}

/// Name-once means once. The second call refuses even though the first call
/// is the thing that made the track named.
#[tokio::test]
async fn a_second_rename_by_the_same_planner_refuses() {
    let boot = boot_unnamed().await;
    let identity = planner_identity(&boot);

    call_tool(
        &boot,
        TOOL_TRACK_RENAME,
        identity.clone(),
        json!({ "title": "first" }),
    )
    .await
    .expect("first name lands");
    let out = call_tool(
        &boot,
        TOOL_TRACK_RENAME,
        identity,
        json!({ "title": "second" }),
    )
    .await
    .expect("second call refuses rather than erroring");

    assert_eq!(
        out.get("refused").and_then(Value::as_str),
        Some("already_named")
    );
    assert_eq!(track_title(&boot).await, "first");
}

/// A whitespace-only title never counted as "named" for the guard, so it must
/// not count as "named" for the refusal either — otherwise a track created with
/// `"   "` would be permanently unnameable.
#[tokio::test]
async fn whitespace_only_existing_title_still_counts_as_unnamed() {
    let boot = boot_with("   ", None).await;
    let out = call_tool(
        &boot,
        TOOL_TRACK_RENAME,
        planner_identity(&boot),
        json!({ "title": "a real name" }),
    )
    .await
    .expect("whitespace-only title is not a name");
    assert_eq!(out.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(track_title(&boot).await, "a real name");
}

// ---------------------------------------------------------------------------
// Role gate
// ---------------------------------------------------------------------------

/// The prompt is not the guard. Every non-Planner role is refused by
/// `require_role` before the handler reaches a write, and the message is
/// checked because a malformed-arguments rejection carries the same
/// `-32602` code.
#[tokio::test]
async fn non_planner_roles_are_forbidden() {
    for role in [CardRole::Worker, CardRole::Assistant, CardRole::ReportCard] {
        let boot = boot_unnamed().await;
        let identity = identity_for(&boot, &boot.other_card_id, role, "other-session");
        let err = call_tool(
            &boot,
            TOOL_TRACK_RENAME,
            identity,
            json!({ "title": "not yours to name" }),
        )
        .await
        .expect_err("only a planner card may name a track");
        assert_eq!(err.code, RpcError::INVALID_PARAMS, "role={role:?}");
        assert!(
            err.message.contains("tool requires role"),
            "role={role:?}: expected the role refusal, got {:?}",
            err.message
        );
        assert_eq!(track_title(&boot).await, "", "role={role:?}");
    }
}

// ---------------------------------------------------------------------------
// The two track classes whose names are not the agent's to write
// ---------------------------------------------------------------------------

/// Template tracks are a catalogue the user curates; the names ARE the
/// catalogue entries. The overlay is written through the same
/// `overlay_upsert` production uses on the `as_template` create branch.
#[tokio::test]
async fn template_track_refuses_rename() {
    let boot = boot_unnamed().await;
    boot.repo
        .overlay_upsert(NewOverlay {
            plugin_id: OVERLAY_TEMPLATE_PLUGIN_ID.into(),
            entity_kind: OVERLAY_TEMPLATE_ENTITY_KIND.into(),
            entity_id: boot.track_id.as_str().to_string(),
            kind: OVERLAY_TEMPLATE_KIND.into(),
            payload: template_overlay_payload(),
        })
        .await
        .expect("mark the track as a template");

    let out = call_tool(
        &boot,
        TOOL_TRACK_RENAME,
        planner_identity(&boot),
        json!({ "title": "renaming a template" }),
    )
    .await
    .expect("refusal is a value");
    assert_eq!(
        out.get("refused").and_then(Value::as_str),
        Some("template_track")
    );
    assert_eq!(track_title(&boot).await, "");
}

/// The per-area chat track's name is kernel-owned — the same reason its
/// lifecycle is not user-drivable (`routes::tracks::update_track`).
#[tokio::test]
async fn area_chat_track_refuses_rename() {
    let boot = boot_with("", Some(calm_server::AREA_CHAT_PURPOSE)).await;
    let out = call_tool(
        &boot,
        TOOL_TRACK_RENAME,
        planner_identity(&boot),
        json!({ "title": "renaming the chat track" }),
    )
    .await
    .expect("refusal is a value");
    assert_eq!(
        out.get("refused").and_then(Value::as_str),
        Some("chat_track")
    );
    assert_eq!(track_title(&boot).await, "");
}

// ---------------------------------------------------------------------------
// Argument validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn empty_or_missing_title_is_an_argument_error() {
    for args in [
        json!({}),
        json!({ "title": "" }),
        json!({ "title": "  \t " }),
    ] {
        let boot = boot_unnamed().await;
        let err = call_tool(
            &boot,
            TOOL_TRACK_RENAME,
            planner_identity(&boot),
            args.clone(),
        )
        .await
        .expect_err("a nameless rename is an argument error");
        assert_eq!(err.code, RpcError::INVALID_PARAMS, "args={args}");
        assert_eq!(track_title(&boot).await, "", "args={args}");
    }
}
