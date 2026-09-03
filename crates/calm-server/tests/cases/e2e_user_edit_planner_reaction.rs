//! Issue #247 PR5 — end-to-end coverage for the user-edit → planner-reaction
//! loop.
//!
//! Earlier PRs built the building blocks separately:
//!
//!   * PR2 (`mcp_track_report.rs`) — the planner-MCP `calm.report.{read,
//!     write,edit}` tools persist + emit `TrackReportEdited` with
//!     `author == Planner`.
//!   * PR3 (`rest_track_report.rs`) — the REST `POST /api/tracks/:id/report`
//!     endpoint persists + emits `TrackReportEdited` with
//!     `author == User`.
//!   * PR4 — the UI pencil/edit affordance that drives that REST POST.
//!
//! What's NOT covered by any of those, and what this file pins, is the
//! whole loop end-to-end. #293 cutover: the planner daemon no longer
//! long-polls (`calm.wait_for_events` is gone) — instead the dispatcher
//! subscribes to the track's event stream with a `SubscribeFilter` and
//! pushes the matching `track.report_edited` onto the planner's codex thread
//! as a turn input. This test mirrors that delivery path by subscribing
//! to the same bus (via `EventBus::subscribe_filtered` + the dispatcher's
//! track-scope `SubscribeFilter`) and asserting:
//!
//!   1. PR3's `EditAuthor::User` actually serializes as the lowercase
//!      `"user"` string on the wire that PR5's planner system prompt
//!      instructs the agent to match on.
//!   2. The CRDT merge from a user-write is visible to a subsequent
//!      MCP `calm.report.read` (no read-after-write staleness through
//!      the JSON-cache projection).
//!   3. The same `TrackReportEdited` envelope reaches a track-scoped
//!      subscriber (the dispatcher's filter must accept Card-scoped
//!      events under the track; otherwise the user's edit silently
//!      disappears from the push path).
//!
//! The negative-half also pins that planner-authored writes land with
//! `author == "planner"` — so the planner system prompt's "ignore your own
//! echoes" guidance (and the dispatcher's user-only push gate) is
//! testable for regression. A future serialization break (rename of
//! `EditAuthor` arms, change of `#[serde(rename_all = "lowercase")]`,
//! etc.) would flip both halves at once and fail loud.

#![cfg(unix)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{self, AuthConfig, AuthState, SESSION_COOKIE};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::RepoEventWrite;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_insert_tx, session_mark_track_root_tx};
use calm_server::error::CalmError;
use calm_server::event::{BroadcastEnvelope, EventBus, SubscribeFilter, SubscribeScope};
use calm_server::ids::{AreaId, CardId, TrackId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::track_report::{TOOL_REPORT_READ, TOOL_REPORT_WRITE};
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use calm_server::track_report::TrackReportPayload;
use calm_types::worker::{
    LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession, WorkerSessionId,
    WorkerSessionState,
};
use serde_json::{Value, json};
use tower::ServiceExt;

const PLANNER_SESSION_ID: &str = "planner-session";

// ---------------------------------------------------------------------------
// Fixture — shared `AppState` + `AppContext` so REST writes and MCP
// reads/waits observe the same bus + repo. The two paths in production
// are wired to the same `AppState.events`; we mirror that here by
// cloning the bus into the `AppContext` (`AppContext` is the MCP
// registry's view, `AppState` is the axum router's view).
// ---------------------------------------------------------------------------

struct Boot {
    state: AppState,
    auth_state: AuthState,
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    area_id: AreaId,
    track_id: TrackId,
    planner_card_id: CardId,
    report_card_id: CardId,
    repo: Arc<dyn Repo>,
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

async fn boot() -> Boot {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let area = repo
        .area_create(NewArea {
            name: "e2e-user-edit".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "e2e track".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    // Mint the planner card + track-report card the same way
    // `routes::tracks::create_track` does. The `CardRoleCache` below
    // carries the role pin so the MCP tools' role gate sees Planner /
    // ReportCard correctly.
    let planner_card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
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
    seed_track_root_session(
        repo.as_ref(),
        &track.id,
        &planner_card.id,
        PLANNER_SESSION_ID,
    )
    .await;

    // Shared caches. Both AppState and AppContext must hold the same
    // clones so a write on either side updates a single source of truth.
    let card_role_cache = CardRoleCache::new();
    card_role_cache.insert(planner_card.id.clone(), CardRole::Planner, track.id.clone());
    crate::support::mcp::set_persisted_card_role(
        repo.as_ref(),
        planner_card.id.as_str(),
        CardRole::Planner,
    )
    .await;
    card_role_cache.insert(
        report_card.id.clone(),
        CardRole::ReportCard,
        track.id.clone(),
    );
    let track_area_cache = TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();

    let events = EventBus::new();

    // Build the AppState through `from_parts` with the shared bus and
    // caches. `from_parts` accepts pre-seeded caches via `Option`s, so
    // both the REST router and the MCP context observe the same role /
    // track-area maps.
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-e2e-user-edit"),
            Vec::new(),
            events.clone(),
            calm_server::state::WriteContext::new(
                card_role_cache.clone(),
                track_area_cache.clone(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache.clone()),
        Some(track_area_cache.clone()),
    );
    let auth_state = AuthState::new(AuthConfig {
        username: Some("alice".into()),
        password: Some("hunter2".into()),
        dev_autologin: false,
        display_name: "alice".into(),
    });

    // MCP context — repo + the same bus the REST writes broadcast on,
    // plus the shared role/area caches.
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let ctx = Arc::new(AppContext {
        repo: route_repo,
        track_vcs: repo
            .sqlite_pool()
            .map(calm_truth::track_vcs_repo::SqlxTrackVcsRepo::shared),
        events: events.clone(),
        write: calm_server::state::WriteContext::new(card_role_cache, track_area_cache),
        daemon_token_hash: None,
        gate_logs_dir: std::env::temp_dir().join("neige-test-gate-logs"),
        plugin_host: Arc::new(tokio::sync::OnceCell::new()),
        operation_runtime: Arc::new(tokio::sync::OnceCell::new()),
    });
    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);
    let registry = Arc::new(registry);

    Boot {
        state,
        auth_state,
        ctx,
        registry,
        area_id: area.id,
        track_id: track.id,
        planner_card_id: planner_card.id,
        report_card_id: report_card.id,
        repo,
    }
}

fn planner_identity(b: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: b.planner_card_id.as_str().to_string(),
        role: CardRole::Planner,
        provider: calm_server::session_projection_repo::AgentProvider::Codex,
        session_id: PLANNER_SESSION_ID.to_string(),
        track_id: Some(b.track_id.as_str().to_string()),
        area_id: b.area_id.as_str().to_string(),
        thread_id: "planner-thread".to_string(),
    }
}

/// Build the same protected-router stack `main.rs` assembles (auth
/// middleware outside, actor middleware inside). Order matches the
/// production binary so the REST surface behaves identically.
fn app(state: AppState, auth_state: AuthState) -> axum::Router {
    let protected_rest = routes::protected_router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            auth_state.clone(),
            auth::require_session,
        ));
    let public_rest = routes::public_router();
    let auth_router = auth::router().with_state(auth_state.clone());
    axum::Router::new()
        .merge(protected_rest)
        .merge(public_rest)
        .with_state(state)
        .merge(auth_router)
}

async fn login(app: &axum::Router) -> String {
    let body = serde_json::to_vec(&json!({
        "username": "alice",
        "password": "hunter2",
    }))
    .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "login must succeed");
    let raw = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("Set-Cookie present on login")
        .to_str()
        .unwrap();
    let first = raw.split(';').next().unwrap();
    assert!(first.starts_with(&format!("{SESSION_COOKIE}=")));
    first.to_string()
}

async fn call_mcp(
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

/// The dispatcher's push path subscribes to the track's event stream with
/// a `SubscribeFilter` and reacts to `track.report_edited` (it pushes the
/// matching observation onto the planner's codex thread). This helper mirrors
/// that subscriber: it builds the same track-scoped filter and returns a
/// receiver the test can drain, so we exercise the exact delivery path the
/// dispatcher uses — without booting a real codex thread.
fn subscribe_track_report_edits(
    boot: &Boot,
) -> tokio::sync::broadcast::Receiver<BroadcastEnvelope> {
    boot.state.events.subscribe_filtered()
}

fn track_report_filter(boot: &Boot) -> SubscribeFilter {
    SubscribeFilter {
        scope: SubscribeScope::Track(boot.track_id.clone()),
        include_descendants: true,
        kinds: Some(vec!["track.report_edited".into()]),
    }
}

/// Drain matching `track.report_edited` envelopes off a subscription until
/// `want` of them have arrived or a short deadline expires, rendering each
/// to the same `{ev, data, ...}` wire JSON the dispatcher/WS path produces.
async fn drain_report_edits(
    rx: &mut tokio::sync::broadcast::Receiver<BroadcastEnvelope>,
    filter: &SubscribeFilter,
    want: usize,
) -> Vec<Value> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while out.len() < want {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(env)) => {
                if filter.matches(&env) {
                    let mut v = serde_json::to_value(&env.event).unwrap();
                    if let Value::Object(ref mut m) = v {
                        m.insert("_id".into(), Value::from(env.id));
                    }
                    out.push(v);
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(_)) | Err(_) => break,
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Happy path — the full user-edit → planner-wake → planner-reread loop
// ---------------------------------------------------------------------------

/// The canonical loop the planner system prompt now documents (push model):
///
///   1. Planner seeds a known initial body via `calm.report.write`.
///   2. A track-scoped subscriber (the dispatcher's push filter) observes
///      the planner's own seed write as `author == "planner"`.
///   3. User edits via REST (`POST /api/tracks/:id/report`), appending a
///      sentinel string.
///   4. The same subscriber observes a single `track.report_edited`
///      envelope with `author == "user"` and the sentinel inside
///      `body_after` — this is exactly the event the dispatcher pushes
///      onto the planner's thread as a turn input.
///   5. Planner calls `calm.report.read` and observes the user's body
///      (the sentinel is in the read result, the planner's seed body is
///      gone).
///
/// The assertions at step 4 are load-bearing: PR5's planner prompt tells
/// the agent to gate the "stop and re-read" behavior on `author ==
/// "user"`, and the dispatcher's push gate only fires for user edits, so
/// the lowercase string spelling has to be guaranteed by this path's
/// serde shape.
#[tokio::test]
async fn user_edit_via_rest_reaches_track_subscriber_and_planner_reads_back_user_body() {
    let boot = boot().await;

    // Subscribe to the track's event stream BEFORE any write, exactly as
    // the dispatcher's push path does (it subscribes once at spawn).
    let mut rx = subscribe_track_report_edits(&boot);
    let filter = track_report_filter(&boot);

    // ----- step 1: planner seeds an initial body.
    let initial_body = "# Goal\n\nv0 initial content from planner\n";
    call_mcp(
        &boot,
        TOOL_REPORT_WRITE,
        planner_identity(&boot),
        json!({
            "body": initial_body,
            "summary": "initial summary from planner",
            "message": "seed initial report",
            "if_doc_rev": 0,
        }),
    )
    .await
    .expect("planner seeds initial body");

    // ----- step 2: the subscriber observes the planner's own seed write
    // tagged as Planner. (The dispatcher's push gate would SKIP this — it
    // only pushes user edits — but the envelope still reaches the
    // track-scoped subscriber, which is the surface this asserts.)
    let seed_edits = drain_report_edits(&mut rx, &filter, 1).await;
    assert_eq!(
        seed_edits.len(),
        1,
        "exactly one TrackReportEdited from the planner's seed write; got {seed_edits:?}",
    );
    assert_eq!(
        seed_edits[0]["data"]["author"], "planner",
        "self-write author must be lowercase \"planner\" on the wire (planner prompt matches on it); got {seed_edits:?}",
    );

    // ----- step 3: user edits via REST. We POST through the live
    // axum router so the auth + actor middleware and the
    // `EditAuthor::User` pin in the handler all run end-to-end.
    let user_body = format!("{initial_body}\n## USER ADDED SECTION\nhand-typed line\n");
    let app = app(boot.state.clone(), boot.auth_state.clone());
    let cookie = login(&app).await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{}/report", boot.track_id.as_str()))
                .header("content-type", "application/json")
                .header(header::COOKIE, cookie)
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "summary": "user edited the report",
                        "body": user_body,
                        "ifDocRev": 1,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "REST user edit must succeed");

    // ----- step 4: the track subscriber observes the user's edit — the
    // exact `track.report_edited` the dispatcher pushes onto the planner's
    // thread as a turn input.
    let woken_events = drain_report_edits(&mut rx, &filter, 1).await;
    let user_edits: Vec<_> = woken_events
        .iter()
        .filter(|e| e["data"]["author"].as_str() == Some("user"))
        .collect();
    assert_eq!(
        user_edits.len(),
        1,
        "exactly one user-authored TrackReportEdited reached the subscriber; got {woken_events:?}",
    );
    let user_edit = user_edits[0];
    assert_eq!(
        user_edit["data"]["author"], "user",
        "wire author must be lowercase \"user\" (matches PR5 planner prompt contract); got {user_edit}",
    );
    assert_eq!(
        user_edit["data"]["track_id"],
        boot.track_id.as_str(),
        "track_id on the envelope must match the edited track",
    );
    assert_eq!(
        user_edit["data"]["card_id"],
        boot.report_card_id.as_str(),
        "card_id on the envelope must match the report card",
    );
    // body_before == planner's last seeded body; body_after contains the
    // user's appended section verbatim. Pinning both ends locks in
    // both the CRDT projection of the pre-write state and the
    // post-write state visible to the planner's listener.
    assert_eq!(
        user_edit["data"]["body_before"], initial_body,
        "body_before must reflect the planner's pre-edit body; got {user_edit}",
    );
    let body_after = user_edit["data"]["body_after"]
        .as_str()
        .expect("body_after is a string");
    assert!(
        body_after.contains("USER ADDED SECTION"),
        "body_after must contain the user's sentinel section; got: {body_after}",
    );
    assert_eq!(
        body_after, user_body,
        "body_after must match the REST body byte-for-byte; got: {body_after}",
    );

    // ----- step 5: planner calls report.read and sees the user's body.
    // This is the "treat user's version as ground truth" check from
    // the PR5 prompt: a follow-up read must not see the planner's
    // stale seed body anywhere.
    let read = call_mcp(&boot, TOOL_REPORT_READ, planner_identity(&boot), json!({}))
        .await
        .expect("planner reads back the user's edit");
    let read_body = read["body"].as_str().expect("body is a string");
    assert_eq!(
        read_body, user_body,
        "planner's report.read must see the user's edited body verbatim; got: {read_body}",
    );
    assert!(
        read_body.contains("USER ADDED SECTION"),
        "planner's read result includes the user's sentinel; got: {read_body}",
    );
    assert_eq!(
        read["summary"], "user edited the report",
        "planner's read result includes the user's summary",
    );

    // Belt-and-suspenders: persisted DB state matches.
    let card = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let payload: TrackReportPayload = serde_json::from_value(card.payload).unwrap();
    assert_eq!(payload.body, user_body, "DB row reflects the user's edit");
    assert_eq!(payload.summary, "user edited the report");
}

// ---------------------------------------------------------------------------
// Negative half — planner's own writes echo back as `author == "planner"`
// ---------------------------------------------------------------------------

/// PR5's planner system prompt tells the agent to *ignore* `TrackReportEdited`
/// events with `author == "planner"` (they're the agent's own writes echoing
/// back via the event stream — acting on them would burn cycles and
/// risk write loops), and the dispatcher's push gate only forwards
/// user-authored edits for the same reason. This test pins that contract:
/// a planner `report.write` surfaces on the track stream tagged as Planner, with
/// the same wire spelling the prompt's instruction depends on (`"planner"`,
/// not `"Planner"` / `"PLANNER"`).
///
/// A future regression that broke `EditAuthor` serialization (e.g.
/// stripping the `#[serde(rename_all = "lowercase")]` attribute) would
/// flip the user-half test above AND this planner-half test simultaneously
/// — exactly the lockstep we want, so the agent's prompt instructions and
/// the dispatcher's gate stay testable against the wire shape.
#[tokio::test]
async fn planner_self_write_echoes_as_author_planner_on_the_track_stream() {
    let boot = boot().await;

    // Subscribe to the track stream first (as the dispatcher does).
    let mut rx = subscribe_track_report_edits(&boot);
    let filter = track_report_filter(&boot);

    // A priming write, drained off the subscription so the next drain
    // only sees what follows.
    call_mcp(
        &boot,
        TOOL_REPORT_WRITE,
        planner_identity(&boot),
        json!({
            "body": "# Goal\n\npriming body\n",
            "summary": "priming",
            "message": "prime report stream",
            "if_doc_rev": 0,
        }),
    )
    .await
    .expect("priming write ok");
    let primed = drain_report_edits(&mut rx, &filter, 1).await;
    assert_eq!(
        primed.len(),
        1,
        "priming write surfaces once; got {primed:?}"
    );

    // Now: a second planner-authored write. The stream must surface this
    // as `author == "planner"`, NOT `"user"` (which would be a
    // serialization regression — the planner prompt and the dispatcher's
    // push gate would then be unable to distinguish self-echoes from
    // user edits).
    call_mcp(
        &boot,
        TOOL_REPORT_WRITE,
        planner_identity(&boot),
        json!({
            "body": "# Goal\n\nsecond planner write\n",
            "summary": "self echo",
            "message": "second planner report write",
            "if_doc_rev": 1,
        }),
    )
    .await
    .expect("second planner write ok");
    let self_echoes = drain_report_edits(&mut rx, &filter, 1).await;
    assert_eq!(
        self_echoes.len(),
        1,
        "exactly one TrackReportEdited after the planner write; got {self_echoes:?}",
    );
    assert_eq!(
        self_echoes[0]["data"]["author"], "planner",
        "planner-authored echoes MUST surface with the lowercase \"planner\" string; got {self_echoes:?}",
    );
    assert_eq!(
        self_echoes[0]["data"]["track_id"],
        boot.track_id.as_str(),
        "track_id on the envelope must match the edited track",
    );
    assert_eq!(
        self_echoes[0]["data"]["card_id"],
        boot.report_card_id.as_str(),
        "card_id on the envelope must match the report card",
    );
    // No user envelope hiding among the echoes — distinguishing the
    // two halves is the prompt instruction's (and push gate's) whole point.
    assert!(
        self_echoes
            .iter()
            .all(|e| e["data"]["author"].as_str() != Some("user")),
        "planner self-write must not appear as author=user; got {self_echoes:?}",
    );
}
