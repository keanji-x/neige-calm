//! Exact-key replay through the existing crate-private managed ACP start entry.
//! HTTP deduplicates sends and recovery retires receipted claims before starting;
//! neither can submit the same key with a changed identity/revision to this fence.
//! Keep this adversarial check in-crate rather than exposing a fixture start API.
#![cfg(test)]
use super::*;
use crate::acp_planner::config::AcpPlannerHost;
use crate::acp_planner::session::{AcpPlannerSession, SessionParams};
use crate::harness::QueueEntry;
use provider::control::TurnStartOutcome;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
fn requests(root: &tempfile::TempDir, method: &str) -> Vec<Value> {
    std::fs::read_to_string(root.path().join("requests.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|frame| frame["method"] == method)
        .collect()
}
fn launches(root: &tempfile::TempDir) -> usize {
    std::fs::read_to_string(root.path().join("environment.jsonl"))
        .unwrap()
        .lines()
        .count()
}
async fn receipt(repo: &SqlxRepo, worker: &str) -> (String, String, String) {
    sqlx::query_as(
        "SELECT client_id,turn_id,input_json FROM acp_submissions WHERE worker_session_id=?1",
    )
    .bind(worker)
    .fetch_one(repo.pool())
    .await
    .unwrap()
}
use provider::control::{CorrelationKey, InputRequest, TurnEdit, TurnRequest};

fn request(client: &str) -> TurnRequest {
    TurnRequest {
        input: InputRequest {
            items: vec![provider::InputItem::text("receipt input")],
            correlation: CorrelationKey(client.into()),
        },
        selection: provider::TurnModelSelection::inherit(),
        permission: crate::planner_permission_mode::PlannerPermissionMode::Never,
        edit: TurnEdit::NoEdit,
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_same_key_replay_checks_exact_claim_without_spawn_setup_or_prompt() {
    for state in ["sending", "unknown", "completed"] {
        let root = tempfile::tempdir().unwrap();
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let row = owner(&repo, root.path(), AgentProvider::OpenCode).await;
        let host = Arc::new(AcpPlannerHost::new(
            Some(serde_json::from_value(json!({"agents":[{
                "provider":"opencode", "command":"/usr/bin/python3",
                "args":["-u",concat!(env!("CARGO_MANIFEST_DIR"),"/tests/fixtures/acp_planner/agent.py")],
                "env":{"ACP_FIXTURE_ROOT":root.path()},
                "min_agent_version":"1.0.0"
            }]})).unwrap()), root.path(), root.path().join("unused-shim"), root.path().join("mcp.sock")
        ).unwrap());
        let session = AcpPlannerSession::open(SessionParams {
            engine: host.execution.engine().cloned(),
            host,
            provider: AgentProvider::OpenCode,
            repo: repo.clone(),
            seals: Arc::new(crate::thread_seals::ThreadSeals::new()),
            worker_session_id: row.worker.clone(),
            card_id: row.card.clone(),
            track_id: row.track.clone(),
            cwd: root.path().into(),
            instructions: String::new(),
        });
        let (held, _receiver) = crate::harness::held_requests::channel();
        session.mark_installed(held);
        std::fs::write(root.path().join("scenario"), "reply").unwrap();
        let claim = vec![
            QueueEntry::LegacyUser {
                text: "same".into(),
                envelope_id: None,
                message_ids: vec![],
            },
            QueueEntry::user_message("same".into(), None, vec![]),
        ];
        if state == "unknown" {
            std::fs::write(root.path().join("scenario"), "lost").unwrap();
        }
        let pause = crate::acp_planner::test_seams::pause_settlement(&row.worker);
        let TurnStartOutcome::Started { turn } = session
            .start_turn(&row.thread, request("same-key"), &claim)
            .await
            .unwrap()
        else {
            panic!("receipt was not started")
        };
        let turn_id = turn.into_string();
        tokio::time::timeout(Duration::from_secs(20), pause.entered.notified())
            .await
            .unwrap();
        if state != "sending" {
            pause.release.notify_one();
            tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    let stored: String = sqlx::query_scalar(
                        "SELECT state FROM acp_submissions WHERE worker_session_id=?1",
                    )
                    .bind(&row.worker)
                    .fetch_one(repo.pool())
                    .await
                    .unwrap();
                    if stored == state {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .unwrap();
        }
        let stored_state: String =
            sqlx::query_scalar("SELECT state FROM acp_submissions WHERE worker_session_id=?1")
                .bind(&row.worker)
                .fetch_one(repo.pool())
                .await
                .unwrap();
        assert_eq!(stored_state, state);
        let (_, stored_turn, bytes) = receipt(&repo, &row.worker).await;
        assert_eq!(stored_turn, turn_id);
        let input: Value = serde_json::from_str(&bytes).unwrap();
        // Literal independent codec oracle, including a legacy entry with no invented identity.
        let view = claim[1].user_view().unwrap();
        assert_eq!(
            input["queue"],
            json!([
                {"kind":"legacy-user","id":null,"observation":{"type":"user_message","text":"same"},"envelopeId":null,"messageIds":[],"attachments":[],"rev":null,"queuedAtMs":null},
                {"kind":"user","id":claim[1].id(),"observation":{"type":"user_message","text":"same"},"envelopeId":null,"messageIds":claim[1].message_ids(),"attachments":[],"rev":0,"queuedAtMs":view.queued_at_ms}
            ])
        );
        assert_eq!(
            input["input"],
            json!([{"type":"text","text":"receipt input"}])
        );
        assert_eq!(
            input["prompt"].as_array().unwrap().last().unwrap(),
            &json!({"type":"text","text":"receipt input"})
        );
        let before = (
            launches(&root),
            requests(&root, "initialize"),
            requests(&root, "session/new"),
            requests(&root, "session/load"),
            requests(&root, "session/prompt"),
        );
        let TurnStartOutcome::Started { turn } = session
            .start_turn(&row.thread, request("same-key"), &claim)
            .await
            .unwrap()
        else {
            panic!("receipt replay was not started")
        };
        assert_eq!(turn.into_string(), turn_id);
        let mut revised = claim.clone();
        assert!(revised[1].bump_rev_for_restore());
        let error = session
            .start_turn(&row.thread, request("same-key"), &revised)
            .await
            .unwrap_err();
        assert!(format!("{error:?}").contains("ACP receipt ownership or original input changed"));
        let replaced = vec![
            claim[0].clone(),
            QueueEntry::user_message("same".into(), None, vec![]),
        ];
        let error = session
            .start_turn(&row.thread, request("same-key"), &replaced)
            .await
            .unwrap_err();
        assert!(format!("{error:?}").contains("ACP receipt ownership or original input changed"));
        assert_eq!(
            receipt(&repo, &row.worker).await.2,
            bytes,
            "replay preserves exact receipt bytes"
        );
        assert_eq!(
            (
                launches(&root),
                requests(&root, "initialize"),
                requests(&root, "session/new"),
                requests(&root, "session/load"),
                requests(&root, "session/prompt")
            ),
            before
        );
        pause.release.notify_one();
        session.shutdown().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn carrier_quiescence_acp_issuance_before_active_turn_publication() {
    let root = tempfile::tempdir().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let row = owner(&repo, root.path(), AgentProvider::OpenCode).await;
    let host = Arc::new(AcpPlannerHost::new(
            Some(serde_json::from_value(json!({"agents":[{
                "provider":"opencode", "command":"/usr/bin/python3",
                "args":["-u",concat!(env!("CARGO_MANIFEST_DIR"),"/tests/fixtures/acp_planner/agent.py")],
                "env":{"ACP_FIXTURE_ROOT":root.path()},
                "min_agent_version":"1.0.0"
            }]})).unwrap()), root.path(), root.path().join("unused-shim"), root.path().join("mcp.sock")
        ).unwrap());
    let session = AcpPlannerSession::open(SessionParams {
        engine: host.execution.engine().cloned(),
        host,
        provider: AgentProvider::OpenCode,
        repo: repo.clone(),
        seals: Arc::new(crate::thread_seals::ThreadSeals::new()),
        worker_session_id: row.worker.clone(),
        card_id: row.card.clone(),
        track_id: row.track.clone(),
        cwd: root.path().into(),
        instructions: String::new(),
    });
    let (held, _receiver) = crate::harness::held_requests::channel();
    session.mark_installed(held);
    std::fs::write(root.path().join("scenario"), "reply").unwrap();

    let pause = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "carrier-quiescence-acp-issuance",
        &row.worker,
        pause.clone(),
    );
    let session = Arc::new(session);
    let idle = session.carrier_idle_for_detach().unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_secs(1),
            session.start_turn(&row.thread, request("parked"), &[])
        )
        .await
        .unwrap()
        .is_err()
    );
    drop(idle);
    let issuing = tokio::spawn({
        let session = session.clone();
        let thread = row.thread.clone();
        async move {
            session
                .start_turn(&thread, request("quiescence"), &[])
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(20), pause.entered.notified())
        .await
        .unwrap();
    assert!(session.active_turn_id_for_thread(&row.thread).is_none());
    assert!(session.carrier_idle_for_detach().is_none());
    let mut tx = repo.pool().begin().await.unwrap();
    assert!(
        crate::db::sqlite::acp_submission_unresolved_tx(&mut tx, &row.worker)
            .await
            .unwrap()
    );
    tx.rollback().await.unwrap();
    pause.release.notify_one();
    assert!(issuing.await.unwrap().is_ok());
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn carrier_quiescence_tool_call_vs_fence() {
    tool_call_vs_fence(None).await;
}
#[tokio::test]
async fn carrier_quiescence_registered_discovery_straddles_retirement() {
    tool_call_vs_fence(Some("probe")).await;
}
#[tokio::test]
async fn carrier_quiescence_plugin_discovery_straddles_retirement() {
    tool_call_vs_fence(Some("plugin_missing")).await;
}
async fn tool_call_vs_fence(straddle: Option<&'static str>) {
    use crate::mcp_server::registry::{
        AppContext, CardIdentity, ConnectionIdentity, ToolDescriptor, ToolRegistry,
    };
    let root = tempfile::tempdir().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let row = owner(&repo, root.path(), AgentProvider::Codex).await;
    let ctx = AppContext::new(
        repo.clone(),
        crate::event::EventBus::new(),
        crate::state::WriteContext::new(
            crate::card_role_cache::CardRoleCache::new(),
            crate::track_area_cache::TrackAreaCache::new(),
        ),
        None,
        Default::default(),
        Default::default(),
        root.path().into(),
    );
    let identity = ConnectionIdentity::CardBound(CardIdentity {
        card_id: row.card.clone().into(),
        role: CardRole::Planner,
        provider: AgentProvider::Codex,
        session_id: row.worker.clone(),
        track_id: Some(row.track),
        area_id: repo
            .card_identity_get_by_session(&row.worker)
            .await
            .unwrap()
            .unwrap()
            .area_id
            .to_string(),
    });
    let entered = Arc::new(tokio::sync::Notify::new());
    let mut registry = ToolRegistry::new();
    registry.register(
        ToolDescriptor {
            name: "probe".into(),
            description: "probe".into(),
            input_schema: json!({"type":"object","properties":{}}),
            annotations: None,
            roles: &[CardRole::Planner],
            listed_for: &[],
        },
        Arc::new({
            let entered = entered.clone();
            move |_, _, _| {
                let entered = entered.clone();
                Box::pin(async move {
                    entered.notify_one();
                    std::future::pending().await
                })
            }
        }),
    );
    let registry = Arc::new(registry);
    let invoke = {
        let ctx = ctx.clone();
        let registry = registry.clone();
        let identity = identity.clone();
        move || {
            let (ctx, registry, identity) = (ctx.clone(), registry.clone(), identity.clone());
            async move {
                crate::mcp_server::transport::call_registered_tool(
                    &ctx,
                    &registry,
                    &identity,
                    None,
                    "probe",
                    json!({}),
                )
                .await
            }
        }
    };
    let call = tokio::spawn(invoke());
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    assert!(ctx.tool_calls.fence(&row.worker).is_err());
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    let guard = ctx.tool_calls.fence(&row.worker).unwrap();
    guard.require_idle().unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), invoke()).await,
        Ok(Err(_))
    ));
    drop(guard);
    assert_eq!(ctx.tool_calls.entry_count(), 0);
    ctx.tool_calls
        .fence(&row.worker)
        .unwrap()
        .require_idle()
        .unwrap();
    assert_eq!(ctx.tool_calls.entry_count(), 0);
    if let Some(name) = straddle {
        // Discovery is provisional even when it returned an active Planner.
        let pause = crate::test_seams::PausePoint::default();
        crate::test_seams::install_pause_for_test(
            "mcp-identity-discovered",
            &row.worker,
            pause.clone(),
        );
        let call = tokio::spawn({
            let (ctx, registry, thread) = (ctx.clone(), registry.clone(), row.thread.clone());
            async move {
                crate::mcp_server::transport::dispatch_tools_call(
                    &ctx,
                    None,
                    json!({"name": name, "_meta": {"threadId": thread}}),
                    &ConnectionIdentity::DaemonTrust,
                    &registry,
                )
                .await
            }
        });
        tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
            .await
            .unwrap();
        let fence = ctx.tool_calls.fence(&row.worker).unwrap();
        sqlx::query("UPDATE worker_sessions SET state='superseded' WHERE id=?1")
            .bind(&row.worker)
            .execute(repo.pool())
            .await
            .unwrap();
        drop(fence);
        pause.release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(1), call).await;
        assert!(
            matches!(result, Ok(Ok(Err(ref e))) if e.code == -32601 && e.message.contains("unknown thread_id")),
            "retired discovery must fail authorization before either dispatch: {result:?}"
        );
        assert_eq!(ctx.tool_calls.entry_count(), 0);
    }
}

#[tokio::test]
async fn carrier_quiescence_retained_completed_start_is_not_pending() {
    use crate::operation::{OperationKey, OperationRepo};
    let root = tempfile::tempdir().unwrap();
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let row = owner(&repo, root.path(), AgentProvider::Codex).await;
    let operations = crate::operation::SqlxOperationRepo::new(repo.db().clone());
    let id = operations.insert_operation("planner-harness-start", OperationKey {
        operation_key: "quiescence".into(), idempotency_key: None, payload_hash: "hash".into(),
    }, json!({"canonical_activation_version":2,"card_id":row.card,"worker_session_id":row.worker})).await.unwrap();
    sqlx::query("UPDATE worker_sessions SET spawn_op_id=?1 WHERE id=?2")
        .bind(&id)
        .bind(&row.worker)
        .execute(repo.pool())
        .await
        .unwrap();
    let mut tx = repo.pool().begin().await.unwrap();
    assert!(
        crate::operation::planner_start_fence::CardStartFence::require_settled_tx(
            &mut tx, &row.card
        )
        .await
        .is_err()
    );
    sqlx::query("UPDATE operations SET phase='succeeded',completed_at_ms=42 WHERE id=?1")
        .bind(&id)
        .execute(&mut *tx)
        .await
        .unwrap();
    crate::operation::planner_start_fence::CardStartFence::require_settled_tx(&mut tx, &row.card)
        .await
        .unwrap();
    let retained: String =
        sqlx::query_scalar("SELECT spawn_op_id FROM worker_sessions WHERE id=?1")
            .bind(&row.worker)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(retained, id);
}
