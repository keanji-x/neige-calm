//! Characterization of the observable write semantics at the three report-write decision points.
//! Every expected value is a literal read off an actual run, not derived from `track_report_origin`.

#![cfg(unix)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{self, AuthConfig, AuthState, SESSION_COOKIE};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::ids::TrackId;
use calm_server::mcp_server::tools::track_report::TOOL_REPORT_WRITE;
use calm_server::mcp_server::tools::track_report_blocks::TOOL_REPORT_BLOCKS_UPSERT;
use calm_server::model::{NewArea, NewCard, NewTrack, TrackLifecycle, TrackPatch};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_report::TrackReportPayload;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tower::ServiceExt;

use crate::mcp_track_report::{
    Boot, assistant_identity, boot, call_tool, planner_identity,
    seed_non_root_session_with_provider,
};
use crate::support::mcp::set_persisted_card_role;
use calm_server::mcp_server::ToolCallIdentity;
use calm_server::model::CardRole;
use calm_server::session_projection_repo::AgentProvider;
use calm_types::worker::WorkerProviderKind;

// Actor and attribution expectations are read back out of the persisted `events` table, not a handler's return value.

/// `(actor, payload)` of every persisted event of `kind`, oldest first, both as raw JSON.
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

/// The single `track.report_edited` row a one-write test must have produced.
async fn only_report_edit(pool: &SqlitePool) -> (Value, Value) {
    let rows = persisted_events(pool, "track.report_edited").await;
    assert_eq!(
        rows.len(),
        1,
        "exactly one report edit expected; got {rows:#?}"
    );
    rows.into_iter().next().expect("checked length")
}

/// `author_plugin_id` is `skip_serializing_if = "Option::is_none"`, so `None` is observable as the key being absent.
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

fn mcp_pool(boot: &Boot) -> SqlitePool {
    boot.repo.sqlite_pool().expect("fixture repo is sqlite")
}

async fn set_lifecycle(boot: &Boot, to: TrackLifecycle) {
    boot.repo
        .track_update(
            boot.track_id.as_str(),
            TrackPatch {
                lifecycle: Some(to),
                ..Default::default()
            },
        )
        .await
        .expect("set fixture lifecycle");
}

async fn lifecycle(boot: &Boot) -> TrackLifecycle {
    boot.repo
        .track_get(boot.track_id.as_str())
        .await
        .expect("track lookup")
        .expect("track row")
        .lifecycle
}

async fn mcp_doc_rev(boot: &Boot) -> u64 {
    calm_server::track_report_read::load_report_read_snapshot(
        boot.repo.as_ref(),
        boot.report_card_id.as_str(),
        calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
    )
    .await
    .expect("read report snapshot")
    .doc_rev
}

#[tokio::test]
async fn mcp_planner_document_write_is_planner_attributed_and_promotes_a_draft() {
    let boot = boot().await;
    set_lifecycle(&boot, TrackLifecycle::Draft).await;
    let pool = mcp_pool(&boot);

    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        planner_identity(&boot),
        json!({
            "body": "# Planner wrote this\n",
            "summary": "planner summary",
            "message": "characterization write",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect("the planner may write its own track's report");

    let (actor, payload) = only_report_edit(&pool).await;
    assert_eq!(
        actor,
        json!({"kind": "AiPlannerSession", "id": "planner-session"})
    );
    assert_attribution(&payload, "planner");

    assert_eq!(lifecycle(&boot).await, TrackLifecycle::Planning);
    let promotions = persisted_events(&pool, "track.lifecycle_changed").await;
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
        Some(&json!("[auto] first planner write"))
    );
}

#[tokio::test]
async fn mcp_assistant_block_write_is_assistant_attributed_and_leaves_a_draft_in_draft() {
    let boot = boot().await;
    set_lifecycle(&boot, TrackLifecycle::Draft).await;
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
    assert_eq!(
        actor,
        json!({"kind": "AiCodexSession", "id": "assistant-session"})
    );
    assert_attribution(&payload, "assistant");

    assert_eq!(
        lifecycle(&boot).await,
        TrackLifecycle::Draft,
        "an assistant write must not walk a Draft track out of Draft"
    );
    assert!(
        persisted_events(&pool, "track.lifecycle_changed")
            .await
            .is_empty(),
        "no lifecycle transition may be logged for an assistant write"
    );
}

/// A retired session is also refused by the session-authority resolution (`SessionNotActive`);
/// only the message assertion says the recorder gate refused first.
#[tokio::test]
async fn mcp_report_write_consults_the_recorder_gate_before_it_commits() {
    let boot = boot().await;
    let pool = mcp_pool(&boot);
    sqlx::query("UPDATE worker_sessions SET state = 'exited' WHERE id = ?1")
        .bind("planner-session")
        .execute(&pool)
        .await
        .expect("retire the planner session");

    let error = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        planner_identity(&boot),
        json!({
            "body": "# Denied\n",
            "summary": "denied",
            "message": "characterization write",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect_err("a retired session's write is refused by the recorder gate");
    assert!(
        error.message.contains("recorder gate denied report_write"),
        "expected the recorder-gate refusal; got {error:?}"
    );

    assert!(
        persisted_events(&pool, "track.report_edited")
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

/// Mint a second track in the fixture's area with its own card in `role` and a live session bound to it,
/// registered with the role cache the way production does at boot. Returns the foreign track's id.
async fn seed_foreign_track_session(boot: &Boot, session_id: &str, role: CardRole) -> TrackId {
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
    let agent_card = boot
        .repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .expect("mint the foreign track's agent card");
    set_persisted_card_role(boot.repo.as_ref(), agent_card.id.as_str(), role).await;
    seed_non_root_session_with_provider(
        boot.repo.as_ref(),
        &track.id,
        &agent_card.id,
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

async fn seed_foreign_track_planner_session(boot: &Boot, session_id: &str) -> TrackId {
    seed_foreign_track_session(boot, session_id, CardRole::Planner).await
}

/// The recorder probe as the sole reason a write is refused: the acting session's card is on the foreign track.
#[tokio::test]
async fn mcp_report_write_is_refused_when_the_recorder_gate_is_the_only_objection() {
    let boot = boot().await;
    let pool = mcp_pool(&boot);
    const FOREIGN_SESSION_ID: &str = "foreign-track-planner-session";
    seed_foreign_track_planner_session(&boot, FOREIGN_SESSION_ID).await;

    let identity = ToolCallIdentity {
        // This track's planner card — so the tool resolves this track's report.
        card_id: boot.planner_card_id.as_str().to_string(),
        role: CardRole::Planner,
        provider: AgentProvider::Codex,
        // …but the acting session is the foreign track's.
        session_id: FOREIGN_SESSION_ID.to_string(),
        track_id: Some(boot.track_id.as_str().to_string()),
        area_id: boot.area_id.as_str().to_string(),
        thread_id: "foreign-planner-thread".to_string(),
    };

    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        identity,
        json!({
            "body": "# Denied\n",
            "summary": "denied",
            "message": "characterization write",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect_err("the recorder gate refuses a session whose card is on another track");

    assert!(
        persisted_events(&pool, "track.report_edited")
            .await
            .is_empty(),
        "a denied write persists no edit"
    );
    assert!(
        persisted_events(&pool, "card.updated").await.is_empty(),
        "a denied write persists no card update either"
    );
    assert_eq!(
        mcp_doc_rev(&boot).await,
        0,
        "the document is untouched by a denied write"
    );
}

#[tokio::test]
async fn mcp_assistant_block_write_from_a_foreign_track_is_refused_without_the_probe_too() {
    let boot = boot().await;
    let pool = mcp_pool(&boot);
    const FOREIGN_SESSION_ID: &str = "foreign-track-assistant-session";
    seed_foreign_track_session(&boot, FOREIGN_SESSION_ID, CardRole::Assistant).await;

    let identity = ToolCallIdentity {
        // This track's assistant card — so the tool resolves this track's report.
        card_id: boot.assistant_card_id.as_str().to_string(),
        role: CardRole::Assistant,
        provider: AgentProvider::Codex,
        // …but the acting session is the foreign track's.
        session_id: FOREIGN_SESSION_ID.to_string(),
        track_id: Some(boot.track_id.as_str().to_string()),
        area_id: boot.area_id.as_str().to_string(),
        thread_id: "foreign-assistant-thread".to_string(),
    };

    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        identity,
        json!({
            "kind": "prose",
            "markdown": "# Denied\n",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect_err("an assistant session on another track may not write this report");

    assert!(
        persisted_events(&pool, "track.report_edited")
            .await
            .is_empty(),
        "a denied write persists no edit"
    );
    assert_eq!(
        mcp_doc_rev(&boot).await,
        0,
        "the document is untouched by a denied write"
    );
}

/// One write, two probe consultations: the requested transition is gated as `TrackLifecycle` first, then the report edit as `ReportWrite`.
#[tokio::test]
async fn mcp_report_write_with_a_lifecycle_is_gated_on_the_track_lifecycle_leg_first() {
    let boot = boot().await;
    let pool = mcp_pool(&boot);
    const FOREIGN_SESSION_ID: &str = "foreign-track-planner-session";
    seed_foreign_track_planner_session(&boot, FOREIGN_SESSION_ID).await;
    // The fixture track is already Planning, so `dispatching` is a real transition.
    assert_eq!(lifecycle(&boot).await, TrackLifecycle::Planning);

    let identity = ToolCallIdentity {
        session_id: FOREIGN_SESSION_ID.to_string(),
        thread_id: "foreign-planner-lifecycle-thread".to_string(),
        ..planner_identity(&boot)
    };

    let error = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        identity,
        json!({
            "body": "# Denied\n",
            "summary": "denied",
            "message": "characterization write",
            "lifecycle": "dispatching",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect_err("the recorder gate refuses a session whose card is on another track");
    assert!(
        error
            .message
            .contains("recorder gate denied track_lifecycle"),
        "the lifecycle leg must be the one that refused; got {error:?}"
    );

    assert_eq!(
        lifecycle(&boot).await,
        TrackLifecycle::Planning,
        "the refused transition rolled back with the rest of the transaction"
    );
    assert!(
        persisted_events(&pool, "track.lifecycle_changed")
            .await
            .is_empty(),
        "a denied write persists no lifecycle transition"
    );
    assert!(
        persisted_events(&pool, "track.report_edited")
            .await
            .is_empty(),
        "a denied write persists no edit"
    );
    assert_eq!(mcp_doc_rev(&boot).await, 0);
}

#[tokio::test]
async fn mcp_report_write_probe_reads_the_written_track_not_the_callers_claimed_track() {
    let boot = boot().await;
    let pool = mcp_pool(&boot);
    const FOREIGN_SESSION_ID: &str = "foreign-track-planner-session";
    let foreign_track_id = seed_foreign_track_planner_session(&boot, FOREIGN_SESSION_ID).await;
    assert_ne!(foreign_track_id, boot.track_id);

    // The fixture's own planner identity with the acting session and the claimed `track_id` swapped for the far track's.
    let identity = ToolCallIdentity {
        session_id: FOREIGN_SESSION_ID.to_string(),
        track_id: Some(foreign_track_id.as_str().to_string()),
        thread_id: "foreign-planner-claimed-track-thread".to_string(),
        ..planner_identity(&boot)
    };

    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        identity,
        json!({
            "body": "# Denied\n",
            "summary": "denied",
            "message": "characterization write",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect_err("a self-consistent foreign identity may still not write this track's report");

    assert!(
        persisted_events(&pool, "track.report_edited")
            .await
            .is_empty(),
        "a denied write persists no edit"
    );
    assert!(
        persisted_events(&pool, "card.updated").await.is_empty(),
        "a denied write persists no card update either"
    );
    assert_eq!(
        mcp_doc_rev(&boot).await,
        0,
        "the document is untouched by a denied write"
    );
}

#[tokio::test]
async fn mcp_claude_assistant_block_write_is_actored_to_the_claude_session() {
    let boot = boot().await;
    let pool = mcp_pool(&boot);
    const CLAUDE_SESSION_ID: &str = "assistant-claude-session";
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: boot.track_id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "harness_profile": "assistant"}),
        })
        .await
        .expect("mint the claude assistant's card");
    set_persisted_card_role(boot.repo.as_ref(), card.id.as_str(), CardRole::Assistant).await;
    seed_non_root_session_with_provider(
        boot.repo.as_ref(),
        &boot.track_id,
        &card.id,
        CLAUDE_SESSION_ID,
        WorkerProviderKind::Claude,
    )
    .await;
    boot.repo
        .seed_card_role_cache(&boot.card_role_cache)
        .await
        .expect("re-seed the role cache from the cards table");

    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        ToolCallIdentity {
            card_id: card.id.as_str().to_string(),
            role: CardRole::Assistant,
            provider: AgentProvider::Claude,
            session_id: CLAUDE_SESSION_ID.to_string(),
            track_id: Some(boot.track_id.as_str().to_string()),
            area_id: boot.area_id.as_str().to_string(),
            thread_id: "claude-assistant-thread".to_string(),
        },
        json!({
            "kind": "prose",
            "markdown": "# Claude assistant wrote this\n",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect("a claude assistant may write a prose block");

    let (actor, payload) = only_report_edit(&pool).await;
    assert_eq!(
        actor,
        json!({"kind": "AiClaudeSession", "id": "assistant-claude-session"})
    );
    assert_attribution(&payload, "assistant");
}

// `AppState::from_parts` leaves `card_role_cache` and `track_area_cache` empty. Harmless while every
// write here is `ActorId::User`, which `role_gate::enforce_role` admits without consulting either cache.

struct RestBoot {
    router: axum::Router,
    cookie: String,
    repo: Arc<SqlxRepo>,
    track_id: String,
}

impl RestBoot {
    fn pool(&self) -> &SqlitePool {
        self.repo.pool()
    }

    async fn lifecycle(&self) -> TrackLifecycle {
        self.repo
            .track_get(&self.track_id)
            .await
            .expect("track lookup")
            .expect("track row")
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

/// Fresh in-memory server with one area → one track → one track-report card, plus a logged-in owner session.
async fn rest_boot() -> RestBoot {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "report-characterization".into(),
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
    assert_eq!(
        track.lifecycle,
        TrackLifecycle::Draft,
        "a freshly minted track is the Draft precondition these tests need"
    );
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
            std::env::temp_dir().join("calm-plugins-data-report-characterization"),
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

/// Decision point 3 — `POST /api/tracks/{id}/report`.
#[tokio::test]
async fn rest_document_write_is_user_attributed_and_leaves_a_draft_in_draft() {
    let boot = rest_boot().await;
    let response = boot
        .post(
            format!("/api/tracks/{}/report", boot.track_id),
            json!({"summary": "human", "body": "# Human wrote this\n", "ifDocRev": 0}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let (actor, payload) = only_report_edit(boot.pool()).await;
    assert_eq!(actor, json!({"kind": "User"}));
    assert_attribution(&payload, "user");

    assert_eq!(
        boot.lifecycle().await,
        TrackLifecycle::Draft,
        "a user's report write must not promote the track"
    );
    assert!(
        persisted_events(boot.pool(), "track.lifecycle_changed")
            .await
            .is_empty(),
        "no lifecycle transition may be logged for a REST document write"
    );
}

/// Decision point 2 — `POST /api/tracks/{id}/report/blocks`.
#[tokio::test]
async fn rest_block_write_is_user_attributed_and_leaves_a_draft_in_draft() {
    let boot = rest_boot().await;
    let response = boot
        .post(
            format!("/api/tracks/{}/report/blocks", boot.track_id),
            json!({"kind": "prose", "markdown": "# Human block\n", "ifDocRev": 0}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let (actor, payload) = only_report_edit(boot.pool()).await;
    assert_eq!(actor, json!({"kind": "User"}));
    assert_attribution(&payload, "user");

    assert_eq!(
        boot.lifecycle().await,
        TrackLifecycle::Draft,
        "a user's block write must not promote the track"
    );
    assert!(
        persisted_events(boot.pool(), "track.lifecycle_changed")
            .await
            .is_empty(),
        "no lifecycle transition may be logged for a REST block write"
    );
}

/// The track these writes land on has no `worker_sessions` row at all, which is precisely the shape
/// the gate refuses on the MCP side; both REST writes nevertheless commit.
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
            format!("/api/tracks/{}/report", boot.track_id),
            json!({"summary": "human", "body": "# One\n", "ifDocRev": 0}),
        )
        .await;
    assert_eq!(document.status(), StatusCode::OK);
    let block = boot
        .post(
            format!("/api/tracks/{}/report/blocks", boot.track_id),
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
        persisted_events(boot.pool(), "track.report_edited")
            .await
            .len(),
        2,
        "both REST writes committed on a track with no agent session"
    );
}
