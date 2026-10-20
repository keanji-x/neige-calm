//! #2774 — the cutover a new Planner conversation commits: the old session's credentials resolve
//! to nothing and its admitted writes are refused; the system queue and watermark move once; only
//! the cutover's own tables change; a start that fails after the commit never gives the card back.

use std::time::Duration;

use axum::http::StatusCode;
use calm_server::db::prelude::*;
use calm_server::harness::{HarnessPhaseTag, Observation, QueueEntry};
use calm_server::mcp_server::auth::{CardMcpToken, hash_token};
use calm_server::mcp_server::handshake::TOKEN_NOT_RECOGNIZED_CODE;
use calm_server::mcp_server::registry::ToolCallIdentity;
use calm_server::mcp_server::{AppContext, McpServer, build_default_registry};
use calm_server::model::{CardRole, new_id};
use calm_server::session_projection_repo::AgentProvider;
use serde_json::{Value, json};

use crate::planner_first_start::Boot;
use crate::planner_new_conversation::image;
use crate::planner_restart::{hold_next_drain, ordinary_track};
use crate::support::mcp::{connect, handshake, recv_frame, send_frame, tools_call_frame};

/// The tables a cutover writes. Everything else, including tables added later, must be unchanged.
/// `card_mcp_tokens` is the new thread's credential, minted as every fresh start mints it; the
/// `track_vcs_*` tables are the Track history the appended events commit to.
const CUTOVER_TABLES: &[&str] = &[
    "worker_sessions",
    "cards",
    "tracks",
    "planner_conversation_starts",
    "operations",
    "events",
    "card_mcp_tokens",
    "track_vcs_commits",
    "track_vcs_objects",
    "track_vcs_refs",
    "track_vcs_gc_state",
];

async fn events_through(boot: &Boot, id: i64) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT quote(id)||quote(kind)||quote(payload)||quote(actor) FROM events \
          WHERE id <= ?1 ORDER BY id",
    )
    .bind(id)
    .fetch_all(boot.repo.pool())
    .await
    .unwrap()
}

async fn max_event(boot: &Boot) -> i64 {
    sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM events")
        .fetch_one(boot.repo.pool())
        .await
        .unwrap()
}

async fn state_of(boot: &Boot, session: &str) -> String {
    sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id = ?1")
        .bind(session)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap()
}

/// An MCP listener over the boot's own data, trusting `daemon` as the shared daemon's token.
async fn mcp(boot: &Boot, daemon: &str) -> (std::sync::Arc<McpServer>, std::path::PathBuf) {
    let ctx = AppContext::new(
        boot.repo.clone(),
        boot.state.events.clone(),
        calm_server::state::WriteContext::new(
            boot.state.card_role_cache.clone(),
            boot.state.track_area_cache.clone(),
        ),
        Some(hash_token(daemon)),
        std::sync::Arc::new(tokio::sync::OnceCell::new()),
        std::sync::Arc::new(tokio::sync::OnceCell::new()),
        std::env::temp_dir().join("neige-test-gate-logs"),
    );
    let socket = boot._tmp.path().join("kernel.sock");
    let server = McpServer::spawn_with_context(
        ctx,
        socket.clone(),
        std::path::PathBuf::from("/nonexistent-shim-bin"),
        build_default_registry(),
    )
    .await
    .unwrap();
    (server, socket)
}

/// The identity the transport resolves for a call from `session` on `thread` of the Planner card.
async fn planner_identity(boot: &Boot, session: &str, thread: &str) -> ToolCallIdentity {
    let card = boot
        .repo
        .card_get(&boot.planner_card_id)
        .await
        .unwrap()
        .unwrap();
    let track = boot
        .repo
        .track_get(card.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    ToolCallIdentity {
        card_id: card.id.to_string(),
        role: CardRole::Planner,
        provider: AgentProvider::Codex,
        session_id: session.to_string(),
        track_id: Some(card.track_id.to_string()),
        area_id: track.area_id.to_string(),
        thread_id: thread.to_string(),
    }
}

fn rpc_error_code(frame: &Value) -> Option<i64> {
    frame["error"]["code"].as_i64().or_else(|| {
        frame["result"]["isError"]
            .as_bool()
            .filter(|e| *e)
            .map(|_| 0)
    })
}

/// After the cutover the old Codex thread, the old session's token and a connection opened with it
/// resolve to nothing; a write admitted as the old session before the commit is refused with no
/// row and no event.
#[tokio::test]
async fn planner_new_conversation_old_credentials_and_admitted_writes_are_refused() {
    let boot = ordinary_track().await;
    let old = boot.active().await;
    let old_thread = old.thread_id.clone().expect("the old session has a thread");
    let token = CardMcpToken::generate().into_inner();
    let mut tx = boot.repo.pool().begin().await.unwrap();
    calm_server::db::sqlite::session_mcp_token_set_tx(&mut tx, &old.id, &hash_token(&token))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (_server, socket) = mcp(&boot, "daemon-token").await;
    // A card-bound connection, open across the cutover.
    let (mut bound_rd, mut bound_wr) = connect(&socket).await;
    handshake(&mut bound_rd, &mut bound_wr, &token).await;
    let identity = planner_identity(&boot, &old.id, &old_thread).await;

    let (status, body) = boot.new_conversation(&new_id()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let new = boot.active().await;
    let new_thread = new.thread_id.clone().unwrap();

    send_frame(
        &mut bound_wr,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "neige_report_read", "arguments": {}}}),
    )
    .await;
    let reply = recv_frame(&mut bound_rd).await;
    assert_eq!(
        reply["error"]["code"].as_i64(),
        Some(TOKEN_NOT_RECOGNIZED_CODE),
        "the open card-bound connection: {reply}"
    );
    let (mut rd, mut wr) = connect(&socket).await;
    send_frame(
        &mut wr,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2024-11-05", "capabilities": {},
            "clientInfo": {"name": "stale", "version": "0"},
            "_meta": {"dev.neige/auth": {"token": token}}}}),
    )
    .await;
    let reply = recv_frame(&mut rd).await;
    assert_eq!(
        reply["error"]["code"].as_i64(),
        Some(TOKEN_NOT_RECOGNIZED_CODE),
        "the old token: {reply}"
    );
    let (mut rd, mut wr) = connect(&socket).await;
    handshake(&mut rd, &mut wr, "daemon-token").await;
    send_frame(
        &mut wr,
        tools_call_frame(3, "neige_report_read", &old_thread, json!({})),
    )
    .await;
    let reply = recv_frame(&mut rd).await;
    assert!(rpc_error_code(&reply).is_some(), "the old thread: {reply}");
    send_frame(
        &mut wr,
        tools_call_frame(4, "neige_report_read", &new_thread, json!({})),
    )
    .await;
    let reply = recv_frame(&mut rd).await;
    assert!(
        reply.get("error").is_none(),
        "the new thread resolves: {reply}"
    );

    // A Planner write whose identity the transport resolved before the commit.
    let events = max_event(&boot).await;
    let tag = |who: ToolCallIdentity| {
        build_default_registry().lookup("neige_report_tag").unwrap()(
            boot.state.mcp_context(),
            who,
            json!({"path": "report.md", "add": ["stale"]}),
        )
    };
    let tags = async || -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM report_tags")
            .fetch_one(boot.repo.pool())
            .await
            .unwrap()
    };
    let refused = tag(identity.clone()).await;
    assert!(
        refused.is_err(),
        "the superseded session's write is refused"
    );
    assert_eq!(tags().await, 0, "no row");
    assert_eq!(max_event(&boot).await, events, "no event");
    // The same write as the current session lands: the refusal is the session's, not the call's.
    let current = ToolCallIdentity {
        session_id: new.id.clone(),
        thread_id: new_thread.clone(),
        ..identity
    };
    tag(current).await.expect("the new session writes");
    assert_eq!(tags().await, 1);
    boot.shutdown_session(&old.id).await;
    boot.shutdown().await;
}

/// `neige_source_capture` commits rows with no event: a capture whose identity the transport
/// resolved as the old session before the cutover is refused where it commits, on both of its
/// transactions, and the same capture as the current session lands.
#[tokio::test]
async fn planner_new_conversation_refuses_an_admitted_source_capture() {
    let boot = ordinary_track().await;
    let old = boot.active().await;
    let identity = planner_identity(&boot, &old.id, old.thread_id.as_deref().unwrap()).await;
    let capture = |who: ToolCallIdentity, args: Value| {
        build_default_registry()
            .lookup("neige_source_capture")
            .unwrap()(boot.state.mcp_context(), who, args)
    };
    let manual = |title: &str| {
        json!({"provenance": "manual", "title": title,
               "manual": {"text": "alpha beta gamma"}, "quotes": ["alpha"]})
    };
    let rows = async || -> Vec<(String, String)> {
        sqlx::query_as("SELECT source_id, quotes FROM report_sources ORDER BY source_id")
            .fetch_all(boot.repo.pool())
            .await
            .unwrap()
    };
    let captured = capture(identity.clone(), manual("before the cutover"))
        .await
        .expect("premise: the current session captures")
        .into_structured();
    let source_id = captured["source_id"].as_str().unwrap().to_string();
    let before = rows().await;

    let (status, body) = boot.new_conversation(&new_id()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let new = boot.active().await;

    let refused = capture(identity.clone(), manual("after the cutover")).await;
    assert!(refused.is_err(), "the old session's capture: {refused:?}");
    let refused = capture(
        identity.clone(),
        json!({"source_id": source_id, "quotes": ["gamma"]}),
    )
    .await;
    assert!(refused.is_err(), "the old session's quotes: {refused:?}");
    assert_eq!(rows().await, before, "nothing was written");
    // The refusal is the session's, not the call's.
    let current = ToolCallIdentity {
        session_id: new.id.clone(),
        thread_id: new.thread_id.clone().unwrap(),
        ..identity
    };
    capture(current.clone(), manual("after the cutover"))
        .await
        .expect("the new session captures");
    capture(
        current,
        json!({"source_id": source_id, "quotes": ["gamma"]}),
    )
    .await
    .expect("the new session appends quotes");
    assert_eq!(rows().await.len(), before.len() + 1);
    boot.shutdown_session(&old.id).await;
    boot.shutdown().await;
}

/// The whole system queue and its watermark move to the new session once; the old row ends empty
/// and stamped, so nothing can take its queue again.
#[tokio::test]
async fn planner_new_conversation_moves_the_system_queue_and_watermark_exactly_once() {
    let boot = ordinary_track().await;
    let old = boot.active().await;
    boot.without_owner(&old.id).await;
    let mut snapshot = boot.row_snapshot(&old.id).await;
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.push_watermark = 4242;
    snapshot.set_pending_entries(vec![
        QueueEntry::system(
            Observation::TrackGoal {
                text: "the observation that moves".into(),
            },
            Some(4242),
        )
        .unwrap(),
    ]);
    boot.persist(&old.id, "idle", serde_json::to_value(&snapshot).unwrap())
        .await;

    let release = hold_next_drain();
    let (status, body) = boot.new_conversation(&new_id()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let new = boot.active().await;
    let moved = boot.row_snapshot(&new.id).await;
    assert_eq!(
        moved.push_watermark, 4242,
        "the watermark moved with the queue"
    );
    assert_eq!(
        format!("{:?}", moved.pending_entries())
            .matches("the observation that moves")
            .count(),
        1,
        "{:?}",
        moved.pending_entries()
    );
    let emptied = boot.row_snapshot(&old.id).await;
    assert!(
        emptied.pending_entries().is_empty(),
        "the old row holds nothing"
    );
    assert!(boot.harvested_at(&old.id).await.is_some(), "and is stamped");
    assert_eq!(state_of(&boot, &old.id).await, "superseded");
    release.notify_one();
    boot.shutdown().await;
}

/// Only the cutover's own tables change; every earlier event is untouched.
#[tokio::test]
async fn planner_new_conversation_changes_only_the_cutover_tables() {
    let boot = ordinary_track().await;
    let old = boot.active().await;
    let before = image(&boot, CUTOVER_TABLES).await;
    let events = max_event(&boot).await;
    let history = events_through(&boot, events).await;
    let (status, body) = boot.new_conversation(&new_id()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    // The new session's own first writes settle after the answer.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(image(&boot, CUTOVER_TABLES).await, before);
    assert_eq!(
        events_through(&boot, events).await,
        history,
        "events only append"
    );
    let updated: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM events WHERE id > ?1 AND kind = 'card.updated' AND scope_card = ?2",
    )
    .bind(events)
    .bind(&boot.planner_card_id)
    .fetch_one(boot.repo.pool())
    .await
    .unwrap();
    assert!(updated >= 1, "the cutover announces the card");
    boot.shutdown_session(&old.id).await;
    boot.shutdown().await;
}

/// A start that fails after the commit keeps the new session as the card's failed carrier; the
/// old session is never given the card back, and restart recovers the new conversation in place.
#[tokio::test]
async fn planner_new_conversation_failure_after_the_cutover_is_final_and_restart_recovers() {
    let boot = ordinary_track().await;
    let old = boot.active().await;
    boot.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (status, body) = boot.new_conversation(&new_id()).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body={body}");
    let carrier = boot
        .current_session()
        .await
        .expect("the card names a session");
    assert_ne!(carrier, old.id, "the card is not given back");
    assert_eq!(state_of(&boot, &old.id).await, "superseded");
    assert_eq!(state_of(&boot, &carrier).await, "failed");
    assert_eq!(boot.floors().await, 1, "the floor stays");

    let (status, body) = boot.fresh_start("restart").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let recovered = boot.active().await;
    assert_ne!(recovered.id, old.id);
    assert_ne!(recovered.id, carrier);
    assert_eq!(state_of(&boot, &old.id).await, "superseded");
    assert_eq!(boot.floors().await, 1, "restart leaves the floor alone");
    let (status, rows) =
        crate::planner_restart::get(boot.app.clone(), boot.card_uri("planner/conversations")).await;
    assert_eq!(status, StatusCode::OK, "body={rows}");
    boot.shutdown_session(&old.id).await;
    boot.shutdown().await;
}

/// A start whose session is no longer the expected one fails before writing anything.
#[tokio::test]
async fn planner_new_conversation_a_changed_expected_session_fails_the_cas() {
    use calm_server::operation::OperationKey;
    use calm_server::operation::OperationOutcome;
    let boot = ordinary_track().await;
    let old = boot.active().await;
    let mut payload: Value = serde_json::from_str(
        &sqlx::query_scalar::<_, String>(
            // The Track create's own start, the only one so far.
            "SELECT payload_json FROM operations WHERE kind = 'planner-harness-start'",
        )
        .fetch_one(boot.repo.pool())
        .await
        .unwrap(),
    )
    .unwrap();
    payload["force_new_thread"] = json!(true);
    payload["new_conversation"] = json!({"expected_session_id": "not-the-current-session"});
    let before = image(&boot, &["operations"]).await;
    let op = boot
        .state
        .operation_runtime
        .submit(
            "planner-harness-start",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash: new_id(),
            },
            payload,
        )
        .await
        .unwrap();
    let result = boot.state.operation_runtime.wait(&op).await.unwrap();
    match result.outcome {
        OperationOutcome::Failed {
            last_error_class, ..
        } => assert_eq!(last_error_class.as_deref(), Some("conflict")),
        other => panic!("the CAS must fail the start: {other:?}"),
    }
    assert_eq!(
        image(&boot, &["operations"]).await,
        before,
        "nothing written"
    );
    assert_eq!(boot.active().await.id, old.id);
    assert!(boot.state.harness.get(&old.id).is_some());
    boot.shutdown().await;
}
