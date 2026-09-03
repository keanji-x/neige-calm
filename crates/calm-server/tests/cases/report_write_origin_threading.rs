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
//! | `rest_document_write_passes_the_origin_check` | any member of `routes::waves::update_wave_report`'s quadruple changed |
//! | `rest_block_write_passes_the_origin_check` | any member of `routes::wave_report_blocks::commit`'s quadruple changed |
//! | `mcp_lifecycle_transition_consults_the_recorder_gate_on_its_own_leg` | deleting the `WaveLifecycle` `probe.record` block in `wave_report.rs` (the characterization suite's gap 2, item 4) |
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
use calm_server::mcp_server::tools::wave_report::TOOL_REPORT_WRITE;
use calm_server::mcp_server::tools::wave_report_blocks::TOOL_REPORT_BLOCKS_UPSERT;
use calm_server::model::{CardRole, NewCard, NewCove, NewWave, WaveLifecycle};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::session_projection_repo::AgentProvider;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_report::WaveReportPayload;
use calm_types::worker::WorkerProviderKind;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tower::ServiceExt;

use crate::mcp_wave_report::{
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
        sqlx::query_as("SELECT COUNT(*) FROM events WHERE kind = 'wave.report_edited'")
            .fetch_one(pool)
            .await
            .expect("count report edits");
    count
}

async fn doc_rev(boot: &Boot) -> u64 {
    calm_server::wave_report_read::load_report_read_snapshot(
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
/// The check reads `recorder_shadow.is_some()` off the very binding
/// `commit_report_op` hands to `persist_report_with_shadow`, and
/// `policy_for` declares `RecorderRequirement::AgentGate` for **both** agent
/// roles. So a funnel that keeps the probe for `Spec` and passes `None` for
/// `Assistant` no longer writes ungated: it is refused here, and this test is
/// what goes red. The control group cannot see that, because an allowing gate
/// and no gate at all leave identical rows behind.
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

/// Mint a second wave in the fixture's cove with its own Spec card and its own
/// live session bound to that card, then re-seed the role cache the way
/// `AppState::new` does at boot.
///
/// The session is live, card-bound and Spec-roled, so the #770 session
/// authority resolution admits it; what refuses it is `decide_recorder`'s
/// `card.wave_id == wave` clause, because its card lives on the *other* wave.
/// That is the only shape in this fixture where the recorder gate is the sole
/// objection — the same construction
/// `report_write_characterization.rs::seed_foreign_wave_spec_session` uses, and
/// the reason it is duplicated rather than shared is that the control group
/// must stay untouched by this step.
async fn seed_foreign_wave_spec_session(boot: &Boot, session_id: &str) {
    let wave = boot
        .repo
        .wave_create(NewWave {
            template_input: None,
            cove_id: boot.cove_id.clone(),
            title: "foreign wave".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("mint the foreign wave");
    let spec_card = boot
        .repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .expect("mint the foreign wave's spec card");
    set_persisted_card_role(boot.repo.as_ref(), spec_card.id.as_str(), CardRole::Spec).await;
    seed_non_root_session_with_provider(
        boot.repo.as_ref(),
        &wave.id,
        &spec_card.id,
        session_id,
        WorkerProviderKind::Codex,
    )
    .await;
    boot.repo
        .seed_card_role_cache(&boot.card_role_cache)
        .await
        .expect("re-seed the role cache from the cards table");
}

/// The `WaveLifecycle` leg of the recorder probe — gap 2, item 4 of the
/// characterization suite, which records that deleting that leg outright leaves
/// the whole `calm-server` package green.
///
/// Both legs of the probe ask the same gate about the same principal, so they
/// cannot be told apart by the *verdict*. What tells them apart is the decision
/// kind in the refusal message, and the ordering that makes it observable: in
/// `persist_report_with_shadow` the `WaveLifecycle` `record` runs inside the
/// requested-transition branch, **before** the unconditional `ReportWrite`
/// `record`. So a write that (a) requests a lifecycle transition that actually
/// applies and (b) is refused by the gate answers `wave_lifecycle`, and answers
/// `report_write` the moment that block is deleted — which is the mutation this
/// test exists to catch.
///
/// The transition has to really fire for the branch to be entered: this
/// fixture's wave is `Planning`, and `Planning → Dispatching` is legal
/// (`wave_lifecycle::validate_transition`, and the same pair is driven by
/// `mcp_wave_report::write_lifecycle_legal_emits_wave_updated_and_report_events`).
/// The rollback assertions below say the refusal took the whole transaction
/// with it, lifecycle included.
#[tokio::test]
async fn mcp_lifecycle_transition_consults_the_recorder_gate_on_its_own_leg() {
    let boot = boot().await;
    let pool = mcp_pool(&boot);
    const FOREIGN_SESSION_ID: &str = "foreign-wave-spec-session";
    seed_foreign_wave_spec_session(&boot, FOREIGN_SESSION_ID).await;
    assert_eq!(
        boot.repo
            .wave_get(boot.wave_id.as_str())
            .await
            .expect("wave lookup")
            .expect("wave row")
            .lifecycle,
        WaveLifecycle::Planning,
        "the requested transition below is legal only from Planning"
    );

    let identity = ToolCallIdentity {
        // This wave's spec card — so the tool resolves this wave's report…
        card_id: boot.spec_card_id.as_str().to_string(),
        role: CardRole::Spec,
        provider: AgentProvider::Codex,
        // …while the acting session belongs to the foreign wave.
        session_id: FOREIGN_SESSION_ID.to_string(),
        wave_id: Some(boot.wave_id.as_str().to_string()),
        cove_id: boot.cove_id.as_str().to_string(),
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
    .expect_err("the recorder gate refuses a session whose card is on another wave");
    assert!(
        error
            .message
            .contains("recorder gate denied wave_lifecycle"),
        "the lifecycle leg must be the leg that refuses this write; got {error:?}"
    );

    assert_eq!(
        report_edit_count(&pool).await,
        0,
        "a denied write persists no edit"
    );
    assert_eq!(
        boot.repo
            .wave_get(boot.wave_id.as_str())
            .await
            .expect("wave lookup")
            .expect("wave row")
            .lifecycle,
        WaveLifecycle::Planning,
        "the refused transition was rolled back with the rest of the transaction"
    );
    assert_eq!(doc_rev(&boot).await, 0);
}

// ---------------------------------------------------------------------------
// REST — `routes::waves::update_wave_report` and
// `routes::wave_report_blocks::commit`
// ---------------------------------------------------------------------------

struct RestBoot {
    router: axum::Router,
    cookie: String,
    repo: Arc<SqlxRepo>,
    wave_id: String,
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

/// Fresh in-memory server with one cove → one wave → one wave-report card and a
/// logged-in owner session, layered the way `main.rs` layers protected REST:
/// `actor_middleware` inside `auth::require_session`, so the actor extractor and
/// the handlers' `require_rest_user_actor` gate both run.
async fn rest_boot() -> RestBoot {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "origin-threading".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            template_input: None,
            cove_id: cove.id.clone(),
            title: "report wave".into(),
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
        wave_id: wave.id.clone(),
        kind: "wave-report".into(),
        sort: Some(-1.0),
        payload: serde_json::to_value(WaveReportPayload::initial()).unwrap(),
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
                calm_server::wave_cove_cache::WaveCoveCache::new(),
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

    let wave_id = wave.id.as_str().to_string();
    RestBoot {
        router,
        cookie,
        repo,
        wave_id,
    }
}

/// `POST /api/waves/{id}/report` writes with the origin check in the way.
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
            format!("/api/waves/{}/report", boot.wave_id),
            json!({"summary": "human", "body": "# Human wrote this\n", "ifDocRev": 0}),
        )
        .await;
    assert_ok(response).await;
    assert_eq!(boot.report_edits().await, 1);
}

/// `POST /api/waves/{id}/report/blocks` — the other REST door, which chooses
/// its `recorder_shadow` argument itself instead of inheriting
/// `persist_report`'s hardcoded `None`. Same mutation, same red.
#[tokio::test]
async fn rest_block_write_passes_the_origin_check() {
    let boot = rest_boot().await;
    let response = boot
        .post(
            format!("/api/waves/{}/report/blocks", boot.wave_id),
            json!({"kind": "prose", "markdown": "# Human block\n", "ifDocRev": 0}),
        )
        .await;
    assert_ok(response).await;
    assert_eq!(boot.report_edits().await, 1);
}
