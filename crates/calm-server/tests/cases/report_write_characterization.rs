//! #1252 S1 step 1 — characterization of **today's** observable write
//! semantics at three of the six report-write decision points.
//!
//! These tests pin behaviour as it is on `origin/main`, not behaviour as it
//! ought to be. Every expected value below is a literal that was read off an
//! actual run of this suite; none of it is derived from
//! `wave_report_origin`. That is deliberate: S1 step 2 threads a write-origin
//! type through these same call sites, and a test that computed its
//! expectation from that type would agree with any change it made. If one of
//! these assertions goes red, the semantics of a decision point changed —
//! confirm the change was intended before touching the assertion.
//!
//! **Covered: 3 of the 6 decision points.**
//!
//! | decision point | production entry driven here |
//! |---|---|
//! | `decision_sink::CardDecisionSink::commit_report_op` | real MCP tool dispatch (`calm.report.write`, `calm.report.blocks.upsert`) |
//! | `routes::wave_report_blocks::commit` | real axum router, `POST /api/waves/{id}/report/blocks` |
//! | `routes::waves::update_wave_report` | real axum router, `POST /api/waves/{id}/report` |
//!
//! Not covered here, on purpose: `seed_template_wave` and
//! `restamp_template_report_if_placeholder` (being removed under #1300) and
//! `routes::wave_templates::update_wave_template` (#1230; its fate is
//! undecided and it has a known attribution defect under #1291).
//!
//! Per decision point, four things are observed:
//!
//! 1. **actor** — the `events.actor` column on the persisted
//!    `wave.report_edited` row.
//! 2. **attribution** — `WaveReportEdited.author`, plus whether the payload
//!    carries an `author_plugin_id` key at all.
//! 3. **the consequence of `auto_promote_draft`** — not the argument, the
//!    effect: whether a wave sitting in `Draft` is walked to `Planning` by
//!    the write, and what lifecycle event that produces.
//! 4. **the recorder probe** — whether the write consults the recorder gate.
//!
//! ## What the recorder coverage here can and cannot claim
//!
//! `persist_report_with_shadow` takes `recorder_shadow: Option<Arc<dyn
//! RecorderShadowProbe>>` and calls `probe.record(...)` *inside* the write
//! transaction, before any row is written and long before commit. The MCP
//! funnel builds its probe inline
//! (`decision_sink.rs`, `CardDecisionSinkRecorderShadowProbe`), and the
//! trait is `pub(crate)`, so an out-of-crate test cannot substitute a
//! counting double on the production assembly path. Exact invocation counts
//! (one `ReportWrite` per write, plus one `WaveLifecycle` when a requested
//! transition actually fires) are therefore **not** asserted below; that
//! needs a production seam and is reported separately.
//!
//! What *is* asserted, deterministically and from the production boundary:
//!
//! * the MCP path does consult the gate, and does so before commit — a
//!   denying gate leaves zero events and an unchanged document
//!   (`mcp_report_write_consults_the_recorder_gate_before_it_commits`);
//! * neither REST path consults it — both succeed on a wave with no agent
//!   session at all, which is exactly the situation the gate refuses on the
//!   MCP side (`rest_report_writes_do_not_consult_the_recorder_gate`).

#![cfg(unix)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{self, AuthConfig, AuthState, SESSION_COOKIE};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::mcp_server::tools::wave_report::TOOL_REPORT_WRITE;
use calm_server::mcp_server::tools::wave_report_blocks::TOOL_REPORT_BLOCKS_UPSERT;
use calm_server::model::{NewCard, NewCove, NewWave, WaveLifecycle, WavePatch};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_report::WaveReportPayload;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tower::ServiceExt;

use crate::mcp_wave_report::{Boot, assistant_identity, boot, call_tool, spec_identity};

// ---------------------------------------------------------------------------
// Observation helpers — every expectation in this file is read back out of
// the persisted `events` table, not out of a handler's return value. The
// persisted row is what the audit log, the goldens, and the spec-wake
// decision all read.
// ---------------------------------------------------------------------------

/// `(actor, payload)` of every persisted event of `kind`, oldest first, both
/// as raw JSON. `events.actor` is stored as the serialized `ActorId` and
/// `events.payload` as the variant's content object, so comparing JSON here
/// keeps the expectations literal.
async fn persisted_events(pool: &SqlitePool, kind: &str) -> Vec<(Value, Value)> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT actor, payload FROM events WHERE kind = ?1 ORDER BY id ASC")
            .bind(kind)
            .fetch_all(pool)
            .await
            .expect("read persisted events");
    rows.into_iter()
        .map(|(actor, payload)| {
            (
                serde_json::from_str(&actor).expect("events.actor is JSON"),
                serde_json::from_str(&payload).expect("events.payload is JSON"),
            )
        })
        .collect()
}

/// The single `wave.report_edited` row a one-write test must have produced.
async fn only_report_edit(pool: &SqlitePool) -> (Value, Value) {
    let rows = persisted_events(pool, "wave.report_edited").await;
    assert_eq!(
        rows.len(),
        1,
        "exactly one report edit expected; got {rows:#?}"
    );
    rows.into_iter().next().expect("checked length")
}

/// Attribution as it lands in the log: the `author` value, and whether the
/// payload carries an `author_plugin_id` key at all. The field is
/// `skip_serializing_if = "Option::is_none"`, so `None` is observable as the
/// key being absent — asserting on the key's absence pins exactly that.
fn assert_attribution(payload: &Value, author: &str) {
    assert_eq!(
        payload.get("author"),
        Some(&json!(author)),
        "report edit attribution; payload = {payload:#?}"
    );
    assert!(
        payload.get("author_plugin_id").is_none(),
        "author_plugin_id is absent (the field is `None` and skipped on the \
         wire) for every writer today; payload = {payload:#?}"
    );
}

// ---------------------------------------------------------------------------
// Decision point 1 — `CardDecisionSink::commit_report_op`, entered through
// real MCP tool dispatch. The fixture is the one the other MCP report suites
// use: real registry lookup, real handler, real decision sink, real recorder
// gate against real `worker_sessions` rows.
// ---------------------------------------------------------------------------

fn mcp_pool(boot: &Boot) -> SqlitePool {
    boot.repo.sqlite_pool().expect("fixture repo is sqlite")
}

async fn set_lifecycle(boot: &Boot, to: WaveLifecycle) {
    boot.repo
        .wave_update(
            boot.wave_id.as_str(),
            WavePatch {
                lifecycle: Some(to),
                ..Default::default()
            },
        )
        .await
        .expect("set fixture lifecycle");
}

async fn lifecycle(boot: &Boot) -> WaveLifecycle {
    boot.repo
        .wave_get(boot.wave_id.as_str())
        .await
        .expect("wave lookup")
        .expect("wave row")
        .lifecycle
}

async fn mcp_doc_rev(boot: &Boot) -> u64 {
    calm_server::wave_report_read::load_report_read_snapshot(
        boot.repo.as_ref(),
        boot.report_card_id.as_str(),
    )
    .await
    .expect("read report snapshot")
    .doc_rev
}

/// A spec agent writing the whole document over MCP: attributed to the spec,
/// actored to the spec's *session*, and it promotes a Draft wave.
#[tokio::test]
async fn mcp_spec_document_write_is_spec_attributed_and_promotes_a_draft() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Draft).await;
    let pool = mcp_pool(&boot);

    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "# Spec wrote this\n",
            "summary": "spec summary",
            "message": "characterization write",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect("the spec may write its own wave's report");

    let (actor, payload) = only_report_edit(&pool).await;
    // Observed on this suite's run: the persisted actor is the spec
    // *session*, not the spec card and not a bare `ai:spec`.
    // `SPEC_SESSION_ID` in the shared fixture is the literal
    // `"spec-session"`.
    assert_eq!(
        actor,
        json!({"kind": "AiSpecSession", "id": "spec-session"})
    );
    assert_attribution(&payload, "spec");

    // The consequence of auto-promotion, not the argument: the Draft wave is
    // now Planning, and the transition is logged as the kernel's, with the
    // literal message the auto-promotion helper stamps.
    assert_eq!(lifecycle(&boot).await, WaveLifecycle::Planning);
    let promotions = persisted_events(&pool, "wave.lifecycle_changed").await;
    assert_eq!(
        promotions.len(),
        1,
        "one auto-promotion expected; got {promotions:#?}"
    );
    let (promotion_actor, promotion) = &promotions[0];
    assert_eq!(promotion_actor, &json!({"kind": "Kernel"}));
    assert_eq!(promotion.get("from"), Some(&json!("draft")));
    assert_eq!(promotion.get("to"), Some(&json!("planning")));
    assert_eq!(
        promotion.get("agent_message"),
        Some(&json!("[auto] first spec write"))
    );
}

/// The same funnel entered by an assistant conversation: a different
/// attribution and a different auto-promotion verdict come out of it, and
/// both are decided inside the funnel from the caller's role.
#[tokio::test]
async fn mcp_assistant_block_write_is_assistant_attributed_and_leaves_a_draft_in_draft() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Draft).await;
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
    .expect("an assistant may write a prose block");

    let (actor, payload) = only_report_edit(&pool).await;
    // Observed: an assistant is actored by its *provider* session
    // (`AgentProvider::Codex` in this fixture), not by the spec-session
    // variant. `ASSISTANT_SESSION_ID` is the literal `"assistant-session"`.
    assert_eq!(
        actor,
        json!({"kind": "AiCodexSession", "id": "assistant-session"})
    );
    assert_attribution(&payload, "assistant");

    assert_eq!(
        lifecycle(&boot).await,
        WaveLifecycle::Draft,
        "an assistant write must not walk a Draft wave out of Draft"
    );
    assert!(
        persisted_events(&pool, "wave.lifecycle_changed")
            .await
            .is_empty(),
        "no lifecycle transition may be logged for an assistant write"
    );
}

/// The recorder probe, observed through its only externally visible effect:
/// a denying gate aborts the write.
///
/// The gate denies a session that is no longer an active authority
/// (`decision_gate::decide_recorder`), so flipping the spec session to
/// `exited` — the state production writes when a session's runtime completes
/// (`worker_flow`, the boot reconcile in `lib.rs`) — turns the same call that
/// succeeds above into a refusal.
///
/// The rollback is the timing evidence: the probe runs inside the write
/// transaction, before the document is touched, so a denial leaves the
/// document at its pre-write revision with neither the edit nor the card
/// update persisted. Had the probe run after commit, both would be in the
/// log.
#[tokio::test]
async fn mcp_report_write_consults_the_recorder_gate_before_it_commits() {
    let boot = boot().await;
    let pool = mcp_pool(&boot);
    sqlx::query("UPDATE worker_sessions SET state = 'exited' WHERE id = ?1")
        .bind("spec-session")
        .execute(&pool)
        .await
        .expect("retire the spec session");

    let error = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "# Denied\n",
            "summary": "denied",
            "message": "characterization write",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect_err("a retired session's write is refused by the recorder gate");
    // Literal message observed on this run — it names the decision kind the
    // report-write leg of the probe passes.
    assert!(
        error.message.contains("recorder gate denied report_write"),
        "expected the recorder-gate refusal; got {error:?}"
    );

    assert!(
        persisted_events(&pool, "wave.report_edited")
            .await
            .is_empty(),
        "a denied write persists no edit"
    );
    assert!(
        persisted_events(&pool, "card.updated").await.is_empty(),
        "a denied write persists no card update either — the probe ran \
         inside the transaction, before commit"
    );
    assert_eq!(
        mcp_doc_rev(&boot).await,
        0,
        "the document is untouched by a denied write"
    );
}

// ---------------------------------------------------------------------------
// Decision points 2 and 3 — the two REST channels, entered through the real
// router. The assembly below mirrors `main.rs` (and
// `rest_wave_report.rs::app`): protected REST behind `actor_middleware`
// (innermost) and `auth::require_session` (outermost), so the session gate,
// the actor extractor and the actor pinning inside the handlers all run.
// ---------------------------------------------------------------------------

struct RestBoot {
    router: axum::Router,
    cookie: String,
    repo: Arc<SqlxRepo>,
    wave_id: String,
}

impl RestBoot {
    fn pool(&self) -> &SqlitePool {
        self.repo.pool()
    }

    async fn lifecycle(&self) -> WaveLifecycle {
        self.repo
            .wave_get(&self.wave_id)
            .await
            .expect("wave lookup")
            .expect("wave row")
            .lifecycle
    }

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
}

/// Fresh in-memory server with one cove → one wave → one wave-report card,
/// plus a logged-in owner session. The wave keeps the lifecycle
/// `wave_create` mints, which the assertion below pins as `Draft` — that is
/// the precondition the auto-promotion observations need.
async fn rest_boot() -> RestBoot {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "report-characterization".into(),
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
    assert_eq!(
        wave.lifecycle,
        WaveLifecycle::Draft,
        "a freshly minted wave is the Draft precondition these tests need"
    );
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
            std::env::temp_dir().join("calm-plugins-data-report-characterization"),
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

/// Decision point 3 — `POST /api/waves/{id}/report`.
#[tokio::test]
async fn rest_document_write_is_user_attributed_and_leaves_a_draft_in_draft() {
    let boot = rest_boot().await;
    let response = boot
        .post(
            format!("/api/waves/{}/report", boot.wave_id),
            json!({"summary": "human", "body": "# Human wrote this\n", "ifDocRev": 0}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let (actor, payload) = only_report_edit(boot.pool()).await;
    // Observed: the REST document channel is actored to the bare user, with
    // no session or card id riding along.
    assert_eq!(actor, json!({"kind": "User"}));
    assert_attribution(&payload, "user");

    assert_eq!(
        boot.lifecycle().await,
        WaveLifecycle::Draft,
        "a user's report write must not promote the wave"
    );
    assert!(
        persisted_events(boot.pool(), "wave.lifecycle_changed")
            .await
            .is_empty(),
        "no lifecycle transition may be logged for a REST document write"
    );
}

/// Decision point 2 — `POST /api/waves/{id}/report/blocks`.
#[tokio::test]
async fn rest_block_write_is_user_attributed_and_leaves_a_draft_in_draft() {
    let boot = rest_boot().await;
    let response = boot
        .post(
            format!("/api/waves/{}/report/blocks", boot.wave_id),
            json!({"kind": "prose", "markdown": "# Human block\n", "ifDocRev": 0}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let (actor, payload) = only_report_edit(boot.pool()).await;
    assert_eq!(actor, json!({"kind": "User"}));
    assert_attribution(&payload, "user");

    assert_eq!(
        boot.lifecycle().await,
        WaveLifecycle::Draft,
        "a user's block write must not promote the wave"
    );
    assert!(
        persisted_events(boot.pool(), "wave.lifecycle_changed")
            .await
            .is_empty(),
        "no lifecycle transition may be logged for a REST block write"
    );
}

/// Neither REST channel consults the recorder gate.
///
/// The evidence is the wave these writes land on: it has no
/// `worker_sessions` row at all, which is precisely the shape the gate
/// refuses on the MCP side (`decide_recorder` denies a principal with no
/// session row, and the probe additionally refuses outright when there is no
/// agent principal to gate on). Both REST writes nevertheless commit, so no
/// gated probe ran on either.
///
/// This is a statement about *whether* the gate is consulted, not about an
/// exact invocation count; see the module header.
#[tokio::test]
async fn rest_report_writes_do_not_consult_the_recorder_gate() {
    let boot = rest_boot().await;
    let (sessions,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM worker_sessions")
        .fetch_one(boot.pool())
        .await
        .expect("count sessions");
    assert_eq!(
        sessions, 0,
        "the REST fixture deliberately has no agent session to gate on"
    );

    let document = boot
        .post(
            format!("/api/waves/{}/report", boot.wave_id),
            json!({"summary": "human", "body": "# One\n", "ifDocRev": 0}),
        )
        .await;
    assert_eq!(document.status(), StatusCode::OK);
    let block = boot
        .post(
            format!("/api/waves/{}/report/blocks", boot.wave_id),
            json!({"kind": "prose", "markdown": "# Two\n", "ifDocRev": 1}),
        )
        .await;
    assert_eq!(
        block.status(),
        StatusCode::OK,
        "block write body = {:?}",
        block.into_body().collect().await.unwrap().to_bytes()
    );

    assert_eq!(
        persisted_events(boot.pool(), "wave.report_edited")
            .await
            .len(),
        2,
        "both REST writes committed on a wave with no agent session"
    );
}
