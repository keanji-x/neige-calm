//! #1252 S1 step 2 — evidence that the write-origin equivalence check is
//! *live* at all three report-write decision points, and evidence for the two
//! things `report_write_characterization.rs` registers as gaps in exactly the
//! place this step touches.
//!
//! This file is **not** the control group. `report_write_characterization.rs`
//! is, it is unmodified by this step, and it deliberately mentions none of the
//! new types. This file is the opposite: it exists because the check the step
//! adds is invisible to a suite that only reads persisted rows — a check that
//! silently never ran leaves exactly the same database behind as one that ran
//! and agreed.
//!
//! What each test here is worth is what happens under a mutation, so each one
//! names its mutation:
//!
//! | test | mutation that turns it red |
//! |---|---|
//! | `mcp_spec_write_passes_the_origin_check` | any member of `commit_report_op`'s quadruple changed away from `policy_for`'s answer |
//! | `mcp_assistant_write_keeps_its_own_recorder_probe` | `commit_report_op` passing `recorder_shadow: None` for `CardRole::Assistant` only (the characterization suite's gap 3) |
//! | `rest_document_write_passes_the_origin_check` | any member of `routes::tracks::update_track_report`'s quadruple changed |
//! | `rest_block_write_passes_the_origin_check` | any member of `routes::track_report_blocks::commit`'s quadruple changed |
//! | `mcp_lifecycle_transition_consults_the_recorder_gate_on_its_own_leg` | deleting the `TrackLifecycle` `probe.record` block in `track_report.rs` (the characterization suite's gap 2, item 4) |
//! | `mcp_assistant_write_is_refused_when_the_recorder_gate_denies` | `commit_report_op` passing anything but the probe `verify_legacy_write_arguments` returned — including `if role == Assistant { None } else { .. }` in the argument position. Red as a *different* refusal on this arm (the role gate's `scope.card` also objects); on the **Spec** arm the same mutation lets the write commit — see this test's docs |
//! | `mcp_recorder_probe_gates_on_the_track_being_written` | feeding `AgentOrigin::track_id` (and so the probe's target track) from `identity.track_id` instead of the resolved `track.id` |
//!
//! The first four go red as a **refusal**: `verify_legacy_write_arguments`
//! fails closed with `CalmError::Internal`, so the write never happens and the
//! request is rejected. That is what makes them evidence of the check running
//! rather than of the write working.

#![cfg(unix)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{self, AuthConfig, AuthState, SESSION_COOKIE};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::mcp_server::ToolCallIdentity;
use calm_server::mcp_server::tools::track_report::TOOL_REPORT_WRITE;
use calm_server::mcp_server::tools::track_report_blocks::TOOL_REPORT_BLOCKS_UPSERT;
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack, TrackLifecycle};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::session_projection_repo::AgentProvider;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_report::TrackReportPayload;
use calm_types::worker::WorkerProviderKind;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tower::ServiceExt;

use crate::mcp_track_report::{
    Boot, assistant_identity, boot, call_tool, seed_non_root_session_with_provider, spec_identity,
};
use crate::support::mcp::set_persisted_card_role;

// ---------------------------------------------------------------------------
// MCP — `decision_sink::CardDecisionSink::commit_report_op`
// ---------------------------------------------------------------------------

fn mcp_pool(boot: &Boot) -> SqlitePool {
    boot.repo.sqlite_pool().expect("fixture repo is sqlite")
}

async fn report_edit_count(pool: &SqlitePool) -> i64 {
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM events WHERE kind = 'track.report_edited'")
            .fetch_one(pool)
            .await
            .expect("count report edits");
    count
}

async fn doc_rev(boot: &Boot) -> u64 {
    calm_server::track_report_read::load_report_read_snapshot(
        boot.repo.as_ref(),
        boot.report_card_id.as_str(),
    )
    .await
    .expect("read report snapshot")
    .doc_rev
}

/// The spec arm of the MCP funnel writes with the origin check in the way.
///
/// The write succeeding is the assertion, and it is worth something only
/// because the check fails closed: change any member of `commit_report_op`'s
/// quadruple — actor, author, auto-promote, or the recorder probe — away from
/// what `policy_for(WriteOrigin::Agent { role: Spec, .. })` declares, and this
/// call is refused before the transaction opens, so both assertions below go
/// red.
#[tokio::test]
async fn mcp_spec_write_passes_the_origin_check() {
    let boot = boot().await;
    let pool = mcp_pool(&boot);

    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "# Spec wrote this\n",
            "summary": "spec summary",
            "message": "origin threading write",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect("the spec's own quadruple must equal the policy its origin declares");

    assert_eq!(report_edit_count(&pool).await, 1);
    assert_eq!(doc_rev(&boot).await, 1);
}

/// The assistant arm of the same funnel — the arm gap 3 of the
/// characterization suite says can be silently dropped from the recorder gate.
///
/// `policy_for` declares `RecorderRequirement::AgentGate` for **both** agent
/// roles, and `verify_legacy_write_arguments` builds the probe itself and hands
/// it back, so `commit_report_op` has no probe binding of its own to keep for
/// one role and drop for another.
///
/// What this test asserts is only that the assistant arm still *writes* with
/// the check in the way — a refusal here means the assistant's derived triple
/// stopped matching its origin. It is **not** evidence that the probe reaches
/// the gate: this fixture's gate allows, and the control group cannot tell an
/// allowing gate from no gate at all. That evidence is
/// [`mcp_assistant_write_is_refused_when_the_recorder_gate_denies`], which
/// makes the gate deny.
#[tokio::test]
async fn mcp_assistant_write_keeps_its_own_recorder_probe() {
    let boot = boot().await;
    let pool = mcp_pool(&boot);

    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        assistant_identity(&boot),
        json!({
            "kind": "prose",
            "markdown": "# Assistant wrote this\n",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect("the assistant's own quadruple must equal the policy its origin declares");

    assert_eq!(report_edit_count(&pool).await, 1);
    assert_eq!(doc_rev(&boot).await, 1);
}

/// Mint a second track in the fixture's area with its own card in `role` and its
/// own live session bound to that card, then re-seed the role cache the way
/// `AppState::new` does at boot.
///
/// The session is live and card-bound, so the #770 session authority resolution
/// admits it, and its card's role is whatever the caller asked for, so the role
/// gate admits it too. What refuses it is `decide_recorder`'s
/// `card.track_id == track` clause, because its card lives on the *other* track —
/// so the recorder gate is the first thing to object, and its refusal is the
/// one that comes back.
///
/// It is not the *only* thing that would object: with the probe removed the
/// tool's own `scope.card` check refuses the same call. See
/// [`mcp_assistant_write_is_refused_when_the_recorder_gate_denies`] for why no
/// shape available at this entry point avoids that.
///
/// The `Spec` case is the same construction
/// `report_write_characterization.rs::seed_foreign_track_spec_session` uses; the
/// reason it is duplicated rather than shared is that the control group must
/// stay untouched by this step.
async fn seed_foreign_track_session(
    boot: &Boot,
    session_id: &str,
    role: CardRole,
) -> calm_server::ids::TrackId {
    let track = boot
        .repo
        .track_create(NewTrack {
            template_input: None,
            area_id: boot.area_id.clone(),
            title: "foreign track".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("mint the foreign track");
    let spec_card = boot
        .repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .expect("mint the foreign track's card");
    set_persisted_card_role(boot.repo.as_ref(), spec_card.id.as_str(), role).await;
    seed_non_root_session_with_provider(
        boot.repo.as_ref(),
        &track.id,
        &spec_card.id,
        session_id,
        WorkerProviderKind::Codex,
    )
    .await;
    boot.repo
        .seed_card_role_cache(&boot.card_role_cache)
        .await
        .expect("re-seed the role cache from the cards table");
    track.id
}

/// #1252 S1-2 R1 — the assistant arm really reaches the recorder gate,
/// observed by making that gate deny and reading *which* refusal comes back.
///
/// This is the test the first cut of this step did not have, and its absence is
/// what let a review mutation through. That cut compared
/// `recorder_shadow.is_some()` on a binding; the mutation left the binding
/// alone and wrote
/// `if identity.role == CardRole::Assistant { None } else { recorder_shadow }`
/// in the argument position of `persist_report_with_shadow`. Every test stayed
/// green, because every other assistant test runs against a gate that
/// *allows* — and an allowing gate and no gate at all leave identical rows
/// behind.
///
/// So this one makes the gate say no: the acting session is live and
/// card-bound, and its card just lives on another track, which is
/// `decide_recorder`'s `card.track_id == track` clause. Drop the probe for
/// `Assistant` and the first assertion goes red — the refusal that comes back
/// is no longer the recorder gate's.
///
/// # Why this arm goes red as a different refusal rather than as a landed write
///
/// It does not show the write landing, and that is a fact about **this arm**,
/// not about the boundary. On `calm.report.blocks.upsert` the role gate's
/// `enforce_card_scope` (`calm-truth/src/role_gate.rs`) cross-checks the
/// acting card's home track, so an off-track assistant session is refused there
/// too: remove the probe and the same call comes back as
/// `scope.card mismatch` instead of `recorder gate denied report_write`. That
/// is what the assertion above pins, and it is all it pins.
///
/// **Do not read that as the probe being redundant.** It is not, and the
/// counterexample is in this very file. On the Spec arm —
/// `calm.track.report.write` — `enforce_role_resolving_session` resolves the
/// acting session to `ActorId::AiSpec(card)`, and by that code's own comment
/// (`calm-truth/src/decision_gate.rs`, the `ActorId::AiSpecSession` arm) *"the
/// AiSpec path in enforce_role does not re-check role/scope for ordinary
/// events"*. No scope clause fires, and `decide_recorder`'s
/// `card_track != track` is the **sole** objection. Delete the probe and the
/// write commits.
///
/// Measured, not reasoned: stubbing `recorder_probe_for_agent`'s result to
/// `None` leaves `mcp_integration_suite` at 208 passed / 4 failed, and two of
/// those four fail because the write **landed** with `docRev: 1` —
/// [`mcp_lifecycle_transition_consults_the_recorder_gate_on_its_own_leg`]
/// below, and the control group's own
/// `report_write_characterization::mcp_report_write_is_refused_when_the_recorder_gate_is_the_only_objection`,
/// whose doc already says in as many words that deleting the probe from
/// `CardDecisionSink::commit_report_op` makes that write commit. The other two
/// come back as a different refusal, this test being one of them.
///
/// So the correct statement of gap 3 is: on the Spec arm the recorder probe is
/// the only thing standing between an off-track spec session and another
/// track's report, and a suite that reads only persisted rows cannot see it
/// **because the rows it leaves behind are the rows of a successful write.**
/// Step 3 must not treat the probe as deletable.
#[tokio::test]
async fn mcp_assistant_write_is_refused_when_the_recorder_gate_denies() {
    let boot = boot().await;
    let pool = mcp_pool(&boot);
    const FOREIGN_SESSION_ID: &str = "foreign-track-assistant-session";
    seed_foreign_track_session(&boot, FOREIGN_SESSION_ID, CardRole::Assistant).await;

    let identity = ToolCallIdentity {
        // This track's assistant card — so the tool resolves this track's report…
        card_id: boot.assistant_card_id.as_str().to_string(),
        role: CardRole::Assistant,
        provider: AgentProvider::Codex,
        // …while the acting session belongs to the foreign track.
        session_id: FOREIGN_SESSION_ID.to_string(),
        track_id: Some(boot.track_id.as_str().to_string()),
        area_id: boot.area_id.as_str().to_string(),
        thread_id: "foreign-assistant-thread".to_string(),
    };

    let error = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        identity,
        json!({
            "kind": "prose",
            "markdown": "# Assistant tried to write ungated\n",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect_err("the assistant arm carries a recorder probe, and this gate denies");
    assert!(
        error.message.contains("recorder gate denied report_write"),
        "the refusal must come from the recorder probe on the assistant's own \
         write; got {error:?}"
    );

    assert_eq!(
        report_edit_count(&pool).await,
        0,
        "a denied write persists no edit"
    );
    assert_eq!(doc_rev(&boot).await, 0);
}

/// #1252 S1-2 R1 — the recorder probe gates on the track being **written**, not
/// on the track attached to the caller's identity.
///
/// `AgentOrigin::track_id` used to be inert: the probe's target track was a
/// second expression written next to the origin at the call site, so a bogus
/// track in the origin changed nothing, and pointing the probe at
/// `identity.track_id` while the origin kept `track.id` changed nothing either
/// (the two are equal on every production input). Now the probe is built from
/// the origin — `track_report_origin::verify_legacy_write_arguments` calls
/// `decision_sink::recorder_probe_for_agent` — so there is one track, and this
/// test is where the two candidate sources stop agreeing.
///
/// The identity below claims the **foreign** track while writing **this** track's
/// report, with a session that legitimately lives on this track. Production
/// never produces that pair (`ToolCallIdentity::track_id` and the resolved
/// target both come from `cards.track_id` for the same card), which is precisely
/// why it is constructed here: it is the only input that can tell the two
/// sources apart.
///
/// Correct: the probe targets `track.id`, the session's card lives there, the
/// gate allows, the write lands. Feed `AgentOrigin::track_id` from
/// `identity.track_id` instead and the gate compares this track's card against
/// the foreign track, denies, and this test goes red on the very first
/// assertion.
#[tokio::test]
async fn mcp_recorder_probe_gates_on_the_track_being_written() {
    let boot = boot().await;
    let pool = mcp_pool(&boot);
    let foreign_track =
        seed_foreign_track_session(&boot, "foreign-track-session-unused-here", CardRole::Spec)
            .await;
    assert_ne!(
        foreign_track.as_str(),
        boot.track_id.as_str(),
        "the two candidate track sources have to actually differ"
    );

    let mut identity = spec_identity(&boot);
    identity.track_id = Some(foreign_track.as_str().to_string());

    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        identity,
        json!({
            "body": "# Spec wrote this\n",
            "summary": "spec summary",
            "message": "probe targets the written track",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect("the probe must gate on the track being written, where this session's card lives");

    assert_eq!(report_edit_count(&pool).await, 1);
    assert_eq!(doc_rev(&boot).await, 1);
}

/// The `TrackLifecycle` leg of the recorder probe — gap 2, item 4 of the
/// characterization suite, which records that deleting that leg outright leaves
/// the whole `calm-server` package green.
///
/// Both legs of the probe ask the same gate about the same principal, so they
/// cannot be told apart by the *verdict*. What tells them apart is the decision
/// kind in the refusal message, and the ordering that makes it observable: in
/// `persist_report_with_shadow` the `TrackLifecycle` `record` runs inside the
/// requested-transition branch, **before** the unconditional `ReportWrite`
/// `record`. So a write that (a) requests a lifecycle transition that actually
/// applies and (b) is refused by the gate answers `track_lifecycle`, and answers
/// `report_write` the moment that block is deleted — which is the mutation this
/// test exists to catch.
///
/// The transition has to really fire for the branch to be entered: this
/// fixture's track is `Planning`, and `Planning → Dispatching` is legal
/// (`track_lifecycle::validate_transition`, and the same pair is driven by
/// `mcp_track_report::write_lifecycle_legal_emits_track_updated_and_report_events`).
/// The rollback assertions below say the refusal took the whole transaction
/// with it, lifecycle included.
#[tokio::test]
async fn mcp_lifecycle_transition_consults_the_recorder_gate_on_its_own_leg() {
    let boot = boot().await;
    let pool = mcp_pool(&boot);
    const FOREIGN_SESSION_ID: &str = "foreign-track-spec-session";
    seed_foreign_track_session(&boot, FOREIGN_SESSION_ID, CardRole::Spec).await;
    assert_eq!(
        boot.repo
            .track_get(boot.track_id.as_str())
            .await
            .expect("track lookup")
            .expect("track row")
            .lifecycle,
        TrackLifecycle::Planning,
        "the requested transition below is legal only from Planning"
    );

    let identity = ToolCallIdentity {
        // This track's spec card — so the tool resolves this track's report…
        card_id: boot.spec_card_id.as_str().to_string(),
        role: CardRole::Spec,
        provider: AgentProvider::Codex,
        // …while the acting session belongs to the foreign track.
        session_id: FOREIGN_SESSION_ID.to_string(),
        track_id: Some(boot.track_id.as_str().to_string()),
        area_id: boot.area_id.as_str().to_string(),
        thread_id: "foreign-spec-thread".to_string(),
    };

    let error = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        identity,
        json!({
            "body": "# Denied\n",
            "summary": "denied",
            "message": "lifecycle leg",
            "if_doc_rev": 0,
            "lifecycle": "dispatching"
        }),
    )
    .await
    .expect_err("the recorder gate refuses a session whose card is on another track");
    assert!(
        error
            .message
            .contains("recorder gate denied track_lifecycle"),
        "the lifecycle leg must be the leg that refuses this write; got {error:?}"
    );

    assert_eq!(
        report_edit_count(&pool).await,
        0,
        "a denied write persists no edit"
    );
    assert_eq!(
        boot.repo
            .track_get(boot.track_id.as_str())
            .await
            .expect("track lookup")
            .expect("track row")
            .lifecycle,
        TrackLifecycle::Planning,
        "the refused transition was rolled back with the rest of the transaction"
    );
    assert_eq!(doc_rev(&boot).await, 0);
}

// ---------------------------------------------------------------------------
// REST — `routes::tracks::update_track_report` and
// `routes::track_report_blocks::commit`
// ---------------------------------------------------------------------------

struct RestBoot {
    router: axum::Router,
    cookie: String,
    repo: Arc<SqlxRepo>,
    track_id: String,
}

impl RestBoot {
    async fn post(&self, uri: String, body: Value) -> axum::response::Response {
        self.router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .header(header::COOKIE, self.cookie.clone())
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .expect("router responds")
    }

    async fn report_edits(&self) -> i64 {
        report_edit_count(self.repo.pool()).await
    }
}

/// Assert a 200 and, when it is not one, put the response body in the failure
/// message. A fail-closed origin mismatch answers 500 with the mismatch text in
/// the error body, so this is what makes the REST reds below say *which* field
/// disagreed instead of only `500 != 200`.
async fn assert_ok(response: axum::response::Response) {
    let status = response.status();
    if status == StatusCode::OK {
        return;
    }
    let body = response.into_body().collect().await.unwrap().to_bytes();
    panic!(
        "expected 200, got {status}: {}",
        String::from_utf8_lossy(&body)
    );
}

/// Fresh in-memory server with one area → one track → one track-report card and a
/// logged-in owner session, layered the way `main.rs` layers protected REST:
/// `actor_middleware` inside `auth::require_session`, so the actor extractor and
/// the handlers' `require_rest_user_actor` gate both run.
async fn rest_boot() -> RestBoot {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "origin-threading".into(),
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
    repo.card_create(NewCard {
        track_id: track.id.clone(),
        kind: "track-report".into(),
        sort: Some(-1.0),
        payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
        title: None,
    })
    .await
    .unwrap();

    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-origin-threading"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );
    let auth_state = AuthState::new(AuthConfig {
        username: Some("alice".into()),
        password: Some("hunter2".into()),
        dev_autologin: false,
        display_name: "alice".into(),
    });
    let protected_rest = routes::protected_router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            auth_state.clone(),
            auth::require_session,
        ));
    let router = axum::Router::new()
        .merge(protected_rest)
        .merge(routes::public_router())
        .with_state(state)
        .merge(auth::router().with_state(auth_state));

    let login = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"username": "alice", "password": "hunter2"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK, "fixture login must succeed");
    let cookie = login
        .headers()
        .get(header::SET_COOKIE)
        .expect("Set-Cookie on login")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    assert!(cookie.starts_with(&format!("{SESSION_COOKIE}=")));

    let track_id = track.id.as_str().to_string();
    RestBoot {
        router,
        cookie,
        repo,
        track_id,
    }
}

/// `POST /api/tracks/{id}/report` writes with the origin check in the way.
///
/// A mismatch is `CalmError::Internal`, which this router answers with a 500,
/// so changing any member of that handler's quadruple away from
/// `policy_for(WriteOrigin::RestUser)` turns the status assertion red and
/// leaves no edit behind.
#[tokio::test]
async fn rest_document_write_passes_the_origin_check() {
    let boot = rest_boot().await;
    let response = boot
        .post(
            format!("/api/tracks/{}/report", boot.track_id),
            json!({"summary": "human", "body": "# Human wrote this\n", "ifDocRev": 0}),
        )
        .await;
    assert_ok(response).await;
    assert_eq!(boot.report_edits().await, 1);
}

/// `POST /api/tracks/{id}/report/blocks` — the other REST door, which chooses
/// its `recorder_shadow` argument itself instead of inheriting
/// `persist_report`'s hardcoded `None`. Same mutation, same red.
#[tokio::test]
async fn rest_block_write_passes_the_origin_check() {
    let boot = rest_boot().await;
    let response = boot
        .post(
            format!("/api/tracks/{}/report/blocks", boot.track_id),
            json!({"kind": "prose", "markdown": "# Human block\n", "ifDocRev": 0}),
        )
        .await;
    assert_ok(response).await;
    assert_eq!(boot.report_edits().await, 1);
}
