use super::*;
use crate::db::prelude::*;
use crate::db::sqlite::{SqlxRepo, card_create_with_id_tx, session_start_runtime_tx};
use crate::model::{CardRole, NewArea, NewCard, NewTrack, new_id, now_ms};
use crate::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_types::task_recovery::TaskRecoveryCapability;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

struct Fixture {
    repo: Arc<SqlxRepo>,
    service: RecoveryService,
    session: String,
    track: String,
    card: String,
}
impl Fixture {
    async fn new() -> Self {
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let area = repo
            .area_create(NewArea {
                name: "binding".into(),
                color: "#111111".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                area_id: area.id.clone(),
                title: "binding".into(),
                cwd: "/tmp".into(),
                sort: None,
                template_id: None,
                template_input: None,
                plugin_scope: None,
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let roles = crate::card_role_cache::CardRoleCache::new();
        let areas = crate::track_area_cache::TrackAreaCache::new();
        areas.insert(track.id.clone(), area.id);
        let mut tx = repo.pool().begin().await.unwrap();
        let card = card_create_with_id_tx(
            &mut tx,
            new_id(),
            NewCard {
                track_id: track.id.clone(),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: json!({}),
            },
            CardRole::Planner,
            false,
            &roles,
        )
        .await
        .unwrap();
        let session = new_id();
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: session.clone(),
                card_id: card.id.to_string(),
                kind: WorkerSessionKind::SharedPlanner,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Idle,
                terminal_run_id: None,
                thread_id: Some("thread".into()),
                session_id: None,
                active_turn_id: None,
                handle_state_json: None,
                spawn_op_id: None,
                now_ms: now_ms(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        register(repo.as_ref(), card.id.as_str(), "thread")
            .await
            .unwrap();
        let service = RecoveryService {
            repo: repo.clone(),
            events: EventBus::new(),
            write: WriteContext::new(roles, areas),
        };
        Self {
            repo,
            service,
            session,
            track: track.id.to_string(),
            card: card.id.to_string(),
        }
    }
    async fn prepare(&self) -> String {
        prepare(
            self.repo.as_ref(),
            &self.session,
            &self.track,
            "thread",
            &[crate::codex_appserver::InputItem::text("immutable input")],
            vec![Action {
                key: "a".into(),
                expected_attempt_id: "original-attempt".into(),
                event_id: 1,
                request_key: format!("action-{}", uuid::Uuid::new_v4()),
                capability: TaskRecoveryCapability {
                    allowed: false,
                    code: "explicit_user".into(),
                    reason: "explicit User recovery".into(),
                },
            }],
        )
        .await
        .unwrap()
    }
    async fn bound(&self) -> BoundTurn {
        let id = self.prepare().await;
        bind_turn(self.repo.as_ref(), &id, "turn").await.unwrap();
        store::lookup(self.repo.as_ref(), "thread", "turn")
            .await
            .unwrap()
            .unwrap()
    }
    fn params(&self) -> DynamicToolCallParams {
        DynamicToolCallParams {
            thread_id: "thread".into(),
            turn_id: "turn".into(),
            call_id: "call".into(),
            tool: TOOL.into(),
            namespace: None,
            arguments: json!({"key":"a","reason":"retry unchanged goal"}),
        }
    }
}

#[tokio::test]
async fn acknowledged_turn_binding_is_immutable_and_unknown_turns_have_no_offer() {
    let fx = Fixture::new().await;
    let first = fx.prepare().await;
    assert!(
        store::lookup(fx.repo.as_ref(), "thread", "turn")
            .await
            .unwrap()
            .is_none()
    );
    bind_turn(fx.repo.as_ref(), &first, "turn").await.unwrap();
    bind_turn(fx.repo.as_ref(), &first, "turn").await.unwrap();
    let second = fx.prepare().await;
    assert!(matches!(
        bind_turn(fx.repo.as_ref(), &second, "turn").await,
        Err(CalmError::Conflict(_))
    ));
    assert!(
        store::lookup(fx.repo.as_ref(), "another-thread", "turn")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store::lookup(fx.repo.as_ref(), "thread", "unknown-turn")
            .await
            .unwrap()
            .is_none()
    );
    let bound = store::lookup(fx.repo.as_ref(), "thread", "turn")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bound.actions[0].expected_attempt_id, "original-attempt");
    let input: String =
        sqlx::query_scalar("SELECT input_json FROM planner_recovery_issuances WHERE id=?1")
            .bind(first)
            .fetch_one(fx.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&input).unwrap(),
        json!([{"type":"text","text":"immutable input"}])
    );
}

#[tokio::test]
async fn stale_session_thread_and_role_are_rejected_before_call_provenance() {
    let fx = Fixture::new().await;
    fx.bound().await;
    for (sql, restore) in [
        (
            "UPDATE worker_sessions SET state='superseded'",
            "UPDATE worker_sessions SET state='idle'",
        ),
        (
            "UPDATE worker_sessions SET thread_id='replacement'",
            "UPDATE worker_sessions SET thread_id='thread'",
        ),
        (
            "UPDATE cards SET role='worker' WHERE role='planner'",
            "UPDATE cards SET role='planner' WHERE role='worker'",
        ),
        (
            "UPDATE cards SET session_id=NULL",
            "UPDATE cards SET session_id=(SELECT id FROM worker_sessions LIMIT 1)",
        ),
    ] {
        sqlx::query(sql).execute(fx.repo.pool()).await.unwrap();
        assert!(
            matches!(
                fx.service.execute(&fx.params(), || false).await,
                Err(CalmError::Forbidden(_))
            ),
            "{sql}"
        );
        sqlx::query(restore).execute(fx.repo.pool()).await.unwrap();
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM planner_recovery_calls")
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn call_provenance_replay_does_not_mean_success_and_changed_arguments_conflict() {
    let fx = Fixture::new().await;
    fx.bound().await;
    let mut params = fx.params();
    for _ in 0..2 {
        let err = fx.service.execute(&params, || false).await.unwrap_err();
        assert!(err.to_string().contains("explicit User recovery"));
    }
    params.arguments["reason"] = json!("changed");
    assert!(matches!(
        fx.service.execute(&params, || false).await,
        Err(CalmError::Conflict(_))
    ));
    params.call_id = "different-call".into();
    assert!(matches!(
        fx.service.execute(&params, || false).await,
        Err(CalmError::Forbidden(_))
    ));
    // The ledger did not accept either reason as a successful recovery.
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM task_attempt_allocations")
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn semantic_arguments_cannot_override_transport_identity_or_namespace() {
    let fx = Fixture::new().await;
    fx.bound().await;
    let mut params = fx.params();
    params.arguments["expected_attempt_id"] = json!("forged");
    assert!(fx.service.execute(&params, || false).await.is_err());
    let mut params = fx.params();
    params.namespace = Some("foreign".into());
    assert!(fx.service.execute(&params, || false).await.is_err());
    let mut params = fx.params();
    params.tool = "other".into();
    assert!(fx.service.execute(&params, || false).await.is_err());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM planner_recovery_calls")
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn early_dynamic_call_waits_without_blocking_read_rpc_and_responds_on_original_connection() {
    let fx = Fixture::new().await;
    let issuance = fx.prepare().await;
    let (client, _notifications, mut server) = CodexAppServer::connect_pair_for_test().await;
    let client = Arc::new(client);
    fx.service.install(&client).unwrap();
    let request = json!({"id":"provider-call","method":"item/tool/call","params":{
        "threadId":"thread","turnId":"turn","callId":"call","tool":"Recover",
        "arguments":{"key":"a","reason":"retry unchanged goal"}}});
    server
        .send(Message::Text(request.to_string()))
        .await
        .unwrap();
    // This is an independent, real client RPC while the dynamic job waits on
    // the binding. A read response must arrive without waiting on that job;
    // the production harness integration separately pins the exact turn ACK.
    let peer = async {
        let Message::Text(frame) = server.next().await.unwrap().unwrap() else {
            panic!("RPC text")
        };
        let frame: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(frame["method"], "thread/loaded/list");
        server
            .send(Message::Text(
                json!({"id":frame["id"],"result":{"data":["thread"]}}).to_string(),
            ))
            .await
            .unwrap();
    };
    let (threads, ()) = tokio::join!(client.thread_loaded_list(), peer);
    assert_eq!(threads.unwrap(), vec!["thread".to_string()]);
    bind_turn(fx.repo.as_ref(), &issuance, "turn")
        .await
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(3), server.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Message::Text(response) = response else {
        panic!("response text")
    };
    let response: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["id"], "provider-call");
    assert_eq!(response["result"]["success"], false);
    assert!(response.to_string().contains("explicit User recovery"));
}

#[tokio::test]
async fn unknown_ack_is_bounded_and_cancellation_leaves_no_call_record() {
    let fx = Fixture::new().await;
    fx.prepare().await;
    assert!(matches!(
        fx.service.execute(&fx.params(), || true).await,
        Err(CalmError::ServiceUnavailable(_))
    ));
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        fx.service.execute(&fx.params(), || false),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(CalmError::ServiceUnavailable(_))));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM planner_recovery_calls")
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert!(
        !registered(fx.repo.as_ref(), &fx.card, "old-thread")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn business_recovery_rechecks_bound_thread_inside_its_transaction() {
    let fx = Fixture::new().await;
    let mut bound = fx.bound().await;
    bound.thread_id = "replaced-thread".into();
    let result = crate::task_recovery::recover_failed_task_bound(
        crate::task_recovery::RecoveryContext {
            repo: fx.repo.as_ref(),
            events: &fx.service.events,
            write: &fx.service.write,
        },
        &fx.track,
        "a",
        TaskRecoveryRequest {
            expected_attempt_id: "original-attempt".into(),
            idempotency_key: "bound-action".into(),
            reason: "retry".into(),
        },
        ActorId::AiPlannerSession(fx.session.clone().into()),
        bound,
    )
    .await;
    assert!(
        matches!(result,Err(CalmError::Forbidden(ref reason)) if reason.contains("session/thread")),
        "{result:?}"
    );
}

#[tokio::test]
async fn recovery_tools_are_explicit_in_thread_start_and_absent_on_ordinary_threads() {
    let (client, _notifications, mut peer) = CodexAppServer::connect_pair_for_test().await;
    for tools in [vec![descriptor()], vec![]] {
        let expected = tools.clone();
        let server = async {
            let Message::Text(frame) = peer.next().await.unwrap().unwrap() else {
                panic!("request text")
            };
            let frame: Value = serde_json::from_str(&frame).unwrap();
            assert_eq!(frame["method"], "thread/start");
            if expected.is_empty() {
                assert!(frame["params"].get("dynamicTools").is_none());
            } else {
                assert_eq!(frame["params"]["dynamicTools"], json!(expected));
                assert_eq!(frame["params"]["dynamicTools"][0]["type"], "function");
                assert_eq!(
                    frame["params"]["dynamicTools"][0]["inputSchema"]["required"],
                    json!(["key", "reason"])
                );
            }
            peer.send(Message::Text(
                json!({"id":frame["id"],"result":{"thread":{"id":"minted"}}}).to_string(),
            ))
            .await
            .unwrap();
        };
        let (result, ()) = tokio::join!(
            client.thread_start_with_params_and_tools(
                crate::codex_appserver::ThreadStartParams {
                    cwd: "/tmp".into(),
                    approval_policy: "never".into(),
                    sandbox_mode: "workspace-write".into(),
                    developer_instructions: None,
                    config: None,
                },
                tools
            ),
            server
        );
        assert_eq!(result.unwrap().thread_id(), Some("minted"));
    }
}

#[tokio::test]
async fn semantic_recovery_action_count_boundary_preserves_the_storage_guard() {
    let fx = Fixture::new().await;
    let actions: Vec<_> = (0..129)
        .map(|index| Action {
            key: format!("task-{index}"),
            expected_attempt_id: format!("attempt-{index}"),
            event_id: index + 1,
            request_key: format!("action-{index}"),
            capability: TaskRecoveryCapability {
                allowed: false,
                code: "explicit_user".into(),
                reason: "User recovery required".into(),
            },
        })
        .collect();
    assert!(binding_problem("[]", &actions[..128]).is_none());
    assert!(binding_problem("[]", &actions).unwrap().contains("128"));
    let error = prepare(
        fx.repo.as_ref(),
        &fx.session,
        &fx.track,
        "thread",
        &[crate::codex_appserver::InputItem::text("bounded input")],
        actions,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, CalmError::BadRequest(ref reason) if reason.contains("128")));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM planner_recovery_issuances")
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn semantic_recover_description_states_identical_environment() {
    let description = descriptor()["description"].as_str().unwrap().to_string();
    for needle in [
        "identical execution environment",
        "only the workspace is new",
        "missing capability",
    ] {
        assert!(
            description.contains(needle),
            "Recover description lacks {needle:?}"
        );
    }
}
