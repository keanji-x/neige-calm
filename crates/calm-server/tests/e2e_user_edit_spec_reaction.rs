//! Issue #247 PR5 — end-to-end coverage for the user-edit → spec-reaction
//! loop.
//!
//! Earlier PRs built the building blocks separately:
//!
//!   * PR2 (`mcp_wave_report.rs`) — the spec-MCP `calm.report.{read,
//!     write,edit}` tools persist + emit `WaveReportEdited` with
//!     `author == Spec`.
//!   * PR3 (`rest_wave_report.rs`) — the REST `POST /api/waves/:id/report`
//!     endpoint persists + emits `WaveReportEdited` with
//!     `author == User`.
//!   * PR4 — the UI pencil/edit affordance that drives that REST POST.
//!
//! What's NOT covered by any of those, and what this file pins, is the
//! whole loop end-to-end. #293 cutover: the spec daemon no longer
//! long-polls (`calm.wait_for_events` is gone) — instead the dispatcher
//! subscribes to the wave's event stream with a `SubscribeFilter` and
//! pushes the matching `wave.report_edited` onto the spec's codex thread
//! as a turn input. This test mirrors that delivery path by subscribing
//! to the same bus (via `EventBus::subscribe_filtered` + the dispatcher's
//! wave-scope `SubscribeFilter`) and asserting:
//!
//!   1. PR3's `EditAuthor::User` actually serializes as the lowercase
//!      `"user"` string on the wire that PR5's spec system prompt
//!      instructs the agent to match on.
//!   2. The CRDT merge from a user-write is visible to a subsequent
//!      MCP `calm.report.read` (no read-after-write staleness through
//!      the JSON-cache projection).
//!   3. The same `WaveReportEdited` envelope reaches a wave-scoped
//!      subscriber (the dispatcher's filter must accept Card-scoped
//!      events under the wave; otherwise the user's edit silently
//!      disappears from the push path).
//!
//! The negative-half also pins that spec-authored writes land with
//! `author == "spec"` — so the spec system prompt's "ignore your own
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
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{BroadcastEnvelope, EventBus, SubscribeFilter, SubscribeScope};
use calm_server::ids::{CardId, WaveId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::wave_report::{TOOL_REPORT_READ, TOOL_REPORT_WRITE};
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_cove_cache::WaveCoveCache;
use calm_server::wave_report::WaveReportPayload;
use serde_json::{Value, json};
use tower::ServiceExt;

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
    wave_id: WaveId,
    spec_card_id: CardId,
    report_card_id: CardId,
    repo: Arc<dyn Repo>,
}

async fn boot() -> Boot {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "e2e-user-edit".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "e2e wave".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    // Mint the spec card + wave-report card the same way
    // `routes::waves::create_wave` does. Plain `card_create` here
    // doesn't tag the role; the `CardRoleCache` below carries the role
    // pin so the MCP tools' role gate sees Spec / ReportCard correctly.
    let spec_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
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

    // Shared caches. Both AppState and AppContext must hold the same
    // clones so a write on either side updates a single source of truth.
    let card_role_cache = CardRoleCache::new();
    card_role_cache.insert(spec_card.id.clone(), CardRole::Spec, wave.id.clone());
    card_role_cache.insert(
        report_card.id.clone(),
        CardRole::ReportCard,
        wave.id.clone(),
    );
    let wave_cove_cache = WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();

    let events = EventBus::new();

    // Build the AppState through `from_parts` with the shared bus and
    // caches. `from_parts` accepts pre-seeded caches via `Option`s, so
    // both the REST router and the MCP context observe the same role /
    // wave-cove maps.
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
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache.clone()),
        Some(wave_cove_cache.clone()),
    );
    let auth_state = AuthState::new(AuthConfig {
        username: Some("alice".into()),
        password: Some("hunter2".into()),
        dev_autologin: false,
        display_name: "alice".into(),
    });

    // MCP context — repo + the same bus the REST writes broadcast on,
    // plus the shared role/cove caches.
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let ctx = Arc::new(AppContext {
        repo: route_repo,
        events: events.clone(),
        card_role_cache,
        wave_cove_cache,
    });
    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);
    let registry = Arc::new(registry);

    Boot {
        state,
        auth_state,
        ctx,
        registry,
        wave_id: wave.id,
        spec_card_id: spec_card.id,
        report_card_id: report_card.id,
        repo,
    }
}

fn spec_identity(b: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: b.spec_card_id.as_str().to_string(),
        role: CardRole::Spec,
        wave_id: Some(b.wave_id.as_str().to_string()),
        thread_id: "spec-thread".to_string(),
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

/// The dispatcher's push path subscribes to the wave's event stream with
/// a `SubscribeFilter` and reacts to `wave.report_edited` (it pushes the
/// matching observation onto the spec's codex thread). This helper mirrors
/// that subscriber: it builds the same wave-scoped filter and returns a
/// receiver the test can drain, so we exercise the exact delivery path the
/// dispatcher uses — without booting a real codex thread.
fn subscribe_wave_report_edits(boot: &Boot) -> tokio::sync::broadcast::Receiver<BroadcastEnvelope> {
    boot.state.events.subscribe_filtered()
}

fn wave_report_filter(boot: &Boot) -> SubscribeFilter {
    SubscribeFilter {
        scope: SubscribeScope::Wave(boot.wave_id.clone()),
        include_descendants: true,
        kinds: Some(vec!["wave.report_edited".into()]),
    }
}

/// Drain matching `wave.report_edited` envelopes off a subscription until
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
// Happy path — the full user-edit → spec-wake → spec-reread loop
// ---------------------------------------------------------------------------

/// The canonical loop the spec system prompt now documents (push model):
///
///   1. Spec seeds a known initial body via `calm.report.write`.
///   2. A wave-scoped subscriber (the dispatcher's push filter) observes
///      the spec's own seed write as `author == "spec"`.
///   3. User edits via REST (`POST /api/waves/:id/report`), appending a
///      sentinel string.
///   4. The same subscriber observes a single `wave.report_edited`
///      envelope with `author == "user"` and the sentinel inside
///      `body_after` — this is exactly the event the dispatcher pushes
///      onto the spec's thread as a turn input.
///   5. Spec calls `calm.report.read` and observes the user's body
///      (the sentinel is in the read result, the spec's seed body is
///      gone).
///
/// The assertions at step 4 are load-bearing: PR5's spec prompt tells
/// the agent to gate the "stop and re-read" behavior on `author ==
/// "user"`, and the dispatcher's push gate only fires for user edits, so
/// the lowercase string spelling has to be guaranteed by this path's
/// serde shape.
#[tokio::test]
async fn user_edit_via_rest_reaches_wave_subscriber_and_spec_reads_back_user_body() {
    let boot = boot().await;

    // Subscribe to the wave's event stream BEFORE any write, exactly as
    // the dispatcher's push path does (it subscribes once at spawn).
    let mut rx = subscribe_wave_report_edits(&boot);
    let filter = wave_report_filter(&boot);

    // ----- step 1: spec seeds an initial body.
    let initial_body = "# Goal\n\nv0 initial content from spec\n";
    call_mcp(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": initial_body,
            "summary": "initial summary from spec",
        }),
    )
    .await
    .expect("spec seeds initial body");

    // ----- step 2: the subscriber observes the spec's own seed write
    // tagged as Spec. (The dispatcher's push gate would SKIP this — it
    // only pushes user edits — but the envelope still reaches the
    // wave-scoped subscriber, which is the surface this asserts.)
    let seed_edits = drain_report_edits(&mut rx, &filter, 1).await;
    assert_eq!(
        seed_edits.len(),
        1,
        "exactly one WaveReportEdited from the spec's seed write; got {seed_edits:?}",
    );
    assert_eq!(
        seed_edits[0]["data"]["author"], "spec",
        "self-write author must be lowercase \"spec\" on the wire (spec prompt matches on it); got {seed_edits:?}",
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
                .uri(format!("/api/waves/{}/report", boot.wave_id.as_str()))
                .header("content-type", "application/json")
                .header(header::COOKIE, cookie)
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "summary": "user edited the report",
                        "body": user_body,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "REST user edit must succeed");

    // ----- step 4: the wave subscriber observes the user's edit — the
    // exact `wave.report_edited` the dispatcher pushes onto the spec's
    // thread as a turn input.
    let woken_events = drain_report_edits(&mut rx, &filter, 1).await;
    let user_edits: Vec<_> = woken_events
        .iter()
        .filter(|e| e["data"]["author"].as_str() == Some("user"))
        .collect();
    assert_eq!(
        user_edits.len(),
        1,
        "exactly one user-authored WaveReportEdited reached the subscriber; got {woken_events:?}",
    );
    let user_edit = user_edits[0];
    assert_eq!(
        user_edit["data"]["author"], "user",
        "wire author must be lowercase \"user\" (matches PR5 spec prompt contract); got {user_edit}",
    );
    assert_eq!(
        user_edit["data"]["wave_id"],
        boot.wave_id.as_str(),
        "wave_id on the envelope must match the edited wave",
    );
    assert_eq!(
        user_edit["data"]["card_id"],
        boot.report_card_id.as_str(),
        "card_id on the envelope must match the report card",
    );
    // body_before == spec's last seeded body; body_after contains the
    // user's appended section verbatim. Pinning both ends locks in
    // both the CRDT projection of the pre-write state and the
    // post-write state visible to the spec's listener.
    assert_eq!(
        user_edit["data"]["body_before"], initial_body,
        "body_before must reflect the spec's pre-edit body; got {user_edit}",
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

    // ----- step 5: spec calls report.read and sees the user's body.
    // This is the "treat user's version as ground truth" check from
    // the PR5 prompt: a follow-up read must not see the spec's
    // stale seed body anywhere.
    let read = call_mcp(&boot, TOOL_REPORT_READ, spec_identity(&boot), json!({}))
        .await
        .expect("spec reads back the user's edit");
    let read_body = read["body"].as_str().expect("body is a string");
    assert_eq!(
        read_body, user_body,
        "spec's report.read must see the user's edited body verbatim; got: {read_body}",
    );
    assert!(
        read_body.contains("USER ADDED SECTION"),
        "spec's read result includes the user's sentinel; got: {read_body}",
    );
    assert_eq!(
        read["summary"], "user edited the report",
        "spec's read result includes the user's summary",
    );

    // Belt-and-suspenders: persisted DB state matches.
    let card = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let payload: WaveReportPayload = serde_json::from_value(card.payload).unwrap();
    assert_eq!(payload.body, user_body, "DB row reflects the user's edit");
    assert_eq!(payload.summary, "user edited the report");
}

// ---------------------------------------------------------------------------
// Negative half — spec's own writes echo back as `author == "spec"`
// ---------------------------------------------------------------------------

/// PR5's spec system prompt tells the agent to *ignore* `WaveReportEdited`
/// events with `author == "spec"` (they're the agent's own writes echoing
/// back via the event stream — acting on them would burn cycles and
/// risk write loops), and the dispatcher's push gate only forwards
/// user-authored edits for the same reason. This test pins that contract:
/// a spec `report.write` surfaces on the wave stream tagged as Spec, with
/// the same wire spelling the prompt's instruction depends on (`"spec"`,
/// not `"Spec"` / `"SPEC"`).
///
/// A future regression that broke `EditAuthor` serialization (e.g.
/// stripping the `#[serde(rename_all = "lowercase")]` attribute) would
/// flip the user-half test above AND this spec-half test simultaneously
/// — exactly the lockstep we want, so the agent's prompt instructions and
/// the dispatcher's gate stay testable against the wire shape.
#[tokio::test]
async fn spec_self_write_echoes_as_author_spec_on_the_wave_stream() {
    let boot = boot().await;

    // Subscribe to the wave stream first (as the dispatcher does).
    let mut rx = subscribe_wave_report_edits(&boot);
    let filter = wave_report_filter(&boot);

    // A priming write, drained off the subscription so the next drain
    // only sees what follows.
    call_mcp(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "# Goal\n\npriming body\n",
            "summary": "priming",
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

    // Now: a second spec-authored write. The stream must surface this
    // as `author == "spec"`, NOT `"user"` (which would be a
    // serialization regression — the spec prompt and the dispatcher's
    // push gate would then be unable to distinguish self-echoes from
    // user edits).
    call_mcp(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "# Goal\n\nsecond spec write\n",
            "summary": "self echo",
        }),
    )
    .await
    .expect("second spec write ok");
    let self_echoes = drain_report_edits(&mut rx, &filter, 1).await;
    assert_eq!(
        self_echoes.len(),
        1,
        "exactly one WaveReportEdited after the spec write; got {self_echoes:?}",
    );
    assert_eq!(
        self_echoes[0]["data"]["author"], "spec",
        "spec-authored echoes MUST surface with the lowercase \"spec\" string; got {self_echoes:?}",
    );
    assert_eq!(
        self_echoes[0]["data"]["wave_id"],
        boot.wave_id.as_str(),
        "wave_id on the envelope must match the edited wave",
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
        "spec self-write must not appear as author=user; got {self_echoes:?}",
    );
}
