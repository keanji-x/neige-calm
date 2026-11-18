//! Real ACP stdio through AgentWorkerAdapter, including the durable receipt boundary.
use super::*;
use crate::acp_planner::config::{AcpAgentConfig, AcpPlannerConfig, AcpPlannerHost};
use crate::db::{RepoRead, write_in_tx_typed};
use crate::harness::{HarnessRegistry, wiring::PlannerWiring};
use crate::mcp_server::{AppContext, ToolCallIdentity};
use crate::operation::agent_worker_adapter::AgentWorkerAdapter;
use crate::session_projection_repo::AgentProvider;
use calm_truth::session_repo::SessionRepo;
use std::collections::BTreeMap;

#[tokio::test]
async fn agent_worker_acp_opencode_lifecycle() {
    lifecycle(AgentProvider::OpenCode).await;
}
#[tokio::test]
async fn agent_worker_acp_antigravity_lifecycle() {
    lifecycle(AgentProvider::Antigravity).await;
}
#[tokio::test]
async fn agent_worker_acp_grok_lifecycle() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("calm_server=debug")
        .with_test_writer()
        .try_init();
    lifecycle(AgentProvider::Grok).await;
}

#[derive(Clone, Copy, Debug)]
enum End {
    Unreported,
    Done,
    Cancel,
}

async fn lifecycle(provider: AgentProvider) {
    for end in [End::Unreported, End::Done, End::Cancel] {
        run(provider.clone(), end).await;
    }
}

async fn run(provider: AgentProvider, end: End) {
    let h = worker_lease_harness().await;
    h.repo
        .track_area_cache()
        .seed_from_db(h.repo.pool())
        .await
        .unwrap();
    let execution = crate::test_support::execution::Fixture::new(h.repo.db().clone()).await;
    let fixture = tempfile::tempdir().unwrap();
    std::fs::write(fixture.path().join("scenario"), "worker-held").unwrap();
    let mut env = BTreeMap::from([(
        "ACP_FIXTURE_ROOT".into(),
        fixture.path().display().to_string(),
    )]);
    match provider {
        AgentProvider::Antigravity => env.extend([
            (
                "ACP_FIXTURE_AGENT_NAME".into(),
                "Antigravity ACP Server".into(),
            ),
            ("ACP_FIXTURE_VERSION".into(), "1.5.0".into()),
        ]),
        AgentProvider::Grok => env.extend([
            ("ACP_FIXTURE_IDENTITY".into(), "meta".into()),
            ("ACP_FIXTURE_VENDOR".into(), "xai".into()),
            ("ACP_FIXTURE_AUTH_METHODS".into(), "cached_token".into()),
            ("ACP_FIXTURE_VERSION".into(), "1.0.62".into()),
        ]),
        AgentProvider::OpenCode => {}
        _ => unreachable!(),
    }
    let host = Arc::new(
        AcpPlannerHost::new(
            Some(AcpPlannerConfig {
                agents: vec![AcpAgentConfig {
                    provider: provider.clone(),
                    command: "/usr/bin/python3".into(),
                    args: vec![
                        "-u".into(),
                        concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/tests/fixtures/acp_planner/agent.py"
                        )
                        .into(),
                    ],
                    env,
                    default_permission_mode: None,
                    auth_method: None,
                    min_agent_version: semver::Version::new(1, 0, 0),
                }],
            }),
            &fixture.path().join("data"),
            fixture.path().join("shim"),
            fixture.path().join("mcp.sock"),
            Default::default(),
        )
        .unwrap(),
    );
    host.execution.bind(execution.engine.clone());
    let mut wiring = PlannerWiring::unconfigured_for_test(h.repo.clone());
    wiring.acp = host;
    let daemon = SharedCodexAppServer::new_stub(h.repo.clone());
    let registry = HarnessRegistry::new();
    let adapter = Arc::new(AgentWorkerAdapter::new(
        h.repo.clone(),
        wiring.clone(),
        crate::planner_model::child_selection::ChildSelectionContext {
            availability: Arc::default(),
            claude: wiring.claude.clone(),
            acp: wiring.acp.clone(),
            codex: daemon.clone(),
        },
        crate::agent_backend_pool::codex::CodexBackends::single(daemon),
        registry.clone(),
        h.repo.card_role_cache().clone(),
        h.repo.track_area_cache().clone(),
        Some(fixture.path().join("mcp.sock")),
        h.repo_root.path().into(),
        GateRunWait::DEFAULT,
    ));
    let task = format!("{}:acp", h.track_id);
    h.repo.db().writer().execute(sqlx::query(
        concat!("INSERT INTO tasks(id,track_id,key,kind,provider,goal,context_json,depends_on_json,",
        "status,created_at_ms,updated_at_ms) ",
        "VALUES(?1,?2,'acp','agent',?3,'execute ACP','null','[]','dispatched',1,1)"))
        .bind(&task).bind(&h.track_id).bind(provider.wire_name())).await.unwrap();
    let row = h.repo.task_get(&task).await.unwrap().unwrap();
    let (kind, payload) = crate::scheduler::build_worker_payload(&row).unwrap();
    let key = OperationKey {
        operation_key: new_id(),
        idempotency_key: Some(task.clone()),
        payload_hash: crate::routes::idempotency_key::stable_payload_hash(&payload).unwrap(),
    };
    let operations = Arc::new(SqlxOperationRepo::new(h.repo.db().clone()));
    let completion = OperationCompletionBus::new();
    let ctx = SpawnCtx::new(
        h.repo.clone(),
        operations.clone(),
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new_with_repo(h.repo.clone()),
        h.events.clone(),
        completion.clone(),
    );
    let runtime = Arc::new(
        crate::operation::OperationRuntime::new(
            operations.clone(),
            vec![adapter],
            h.events.clone(),
            completion,
            ctx,
        )
        .await
        .unwrap(),
    );
    let (op, wait) = tokio::time::timeout(
        Duration::from_secs(20),
        runtime.submit_task_bounded(kind, key, payload),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(
            wait,
            crate::operation::OperationWait::Settled(crate::operation::OperationResult {
                outcome: crate::operation::OperationOutcome::Succeeded { .. },
                ..
            })
        ),
        "{provider:?}/{end:?}: {wait:?}"
    );
    let output = operations
        .get_operation(&op)
        .await
        .unwrap()
        .unwrap()
        .tx_output
        .unwrap();
    let card = output.output_string("card_id", "test").unwrap();
    let worker = output.output_string("worker_session_id", "test").unwrap();
    let lease = output.output_string("lease_id", "test").unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if registry
                .live_replies()
                .read(&card.clone().into())
                .items
                .iter()
                .any(|i| i.text == "streamed")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("ACP reply streams through the worker runner");
    let receipt: String =
        sqlx::query_scalar("SELECT state FROM acp_submissions WHERE worker_session_id=?1")
            .bind(&worker)
            .fetch_one(h.repo.pool())
            .await
            .unwrap();
    assert_eq!(receipt, "sending");
    let context = AppContext::new(
        h.repo.clone(),
        h.events.clone(),
        WriteContext::new(
            h.repo.card_role_cache().clone(),
            h.repo.track_area_cache().clone(),
        ),
        None,
        Arc::default(),
        Arc::default(),
        fixture.path().join("gates"),
    );
    context.operation_runtime.set(runtime.clone()).ok().unwrap();
    let track = h.repo.track_get(&h.track_id).await.unwrap().unwrap();
    let session = h
        .repo
        .session_get(&worker.clone().into())
        .await
        .unwrap()
        .unwrap();
    let identity = ToolCallIdentity {
        card_id: card.clone(),
        role: CardRole::Worker,
        provider: provider.clone(),
        session_id: worker.clone(),
        track_id: Some(h.track_id.clone()),
        area_id: track.area_id.to_string(),
        thread_id: session.thread_id.unwrap(),
    };
    match end {
        End::Done => {
            let reply = crate::mcp_server::build_default_registry()
                .lookup("neige_task_done")
                .unwrap()(
                context.clone(),
                identity,
                json!({"attempt_id":task,"result":"ACP done",
                    "commit_message":"test: finish ACP worker"}),
            )
            .await
            .unwrap()
            .into_structured();
            assert_eq!(reply["status"], "report_received", "{reply}");
            std::fs::write(fixture.path().join("release-worker"), "").unwrap();
        }
        End::Cancel => {
            let planner = new_id();
            let planner_worker = new_id();
            let planner_card = planner.clone();
            let ws = planner_worker.clone();
            let planner_track = h.track_id.clone();
            let roles = h.repo.card_role_cache().clone();
            write_in_tx_typed(h.repo.as_ref(), move |tx| {
                Box::pin(async move {
                    crate::db::sqlite::card_create_with_id_tx(
                        tx,
                        planner_card.clone(),
                        crate::model::NewCard {
                            track_id: planner_track.into(),
                            kind: "codex".into(),
                            title: None,
                            sort: None,
                            payload: json!({"schemaVersion":1,"planner_harness":true,
                            "planner_provider":"codex","permission_mode":"never"}),
                        },
                        CardRole::Planner,
                        false,
                        &roles,
                    )
                    .await?;
                    crate::db::sqlite::session_start_runtime_tx(
                        tx,
                        crate::session_projection_repo::WorkerSessionInit::shared_planner(
                            ws,
                            planner_card,
                            AgentProvider::Codex,
                            crate::session_projection_repo::WorkerSessionState::Idle,
                            Some("planner-thread".into()),
                            serde_json::to_value(crate::harness::HarnessSnapshot::initial(
                                0,
                                vec![],
                            ))?,
                            now_ms(),
                        ),
                    )
                    .await?;
                    Ok(())
                })
            })
            .await
            .unwrap();
            let planner_identity = ToolCallIdentity {
                card_id: planner,
                role: CardRole::Planner,
                provider: AgentProvider::Codex,
                session_id: planner_worker,
                thread_id: "planner-thread".into(),
                ..identity
            };
            let reply = crate::mcp_server::build_default_registry()
                .lookup("neige_task_cancel")
                .unwrap()(
                context.clone(),
                planner_identity,
                json!({"key":"acp","message":"Cancel ACP worker"}),
            )
            .await
            .unwrap()
            .into_structured();
            assert_eq!(reply["ok"], true, "{reply}");
        }
        End::Unreported => {
            std::fs::write(fixture.path().join("release-worker"), "").unwrap();
        }
    }
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let state: String =
                sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id=?1")
                    .bind(&lease)
                    .fetch_one(h.repo.pool())
                    .await
                    .unwrap();
            if state == "released" && !registry.has_task_worker(&worker) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|error| {
        panic!("{provider:?}/{end:?}: worker settles and releases its workspace: {error}")
    });
    assert_eq!(
        h.repo
            .session_get(&worker.clone().into())
            .await
            .unwrap()
            .unwrap()
            .state,
        crate::session_projection_repo::WorkerSessionState::Exited
    );
    let requests: Vec<Value> = std::fs::read_to_string(fixture.path().join("requests.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        requests
            .iter()
            .any(|request| request["method"] == "session/new")
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "session/prompt")
            .count(),
        1
    );
    if matches!(end, End::Cancel) {
        assert!(
            requests
                .iter()
                .any(|request| request["method"] == "session/cancel")
        );
    }
    let row = h.repo.task_get(&task).await.unwrap().unwrap();
    assert_eq!(
        row.status,
        match end {
            End::Unreported => crate::model::TaskStatus::Failed,
            End::Done => crate::model::TaskStatus::Done,
            End::Cancel => crate::model::TaskStatus::Canceled,
        }
    );
    if matches!(end, End::Unreported) {
        assert_eq!(
            row.status_detail.as_deref(),
            Some(crate::scheduler::WORKER_TURN_ENDED)
        );
    }
    let view = calm_truth::track_fs_view::TrackFsView::new(h.repo.as_ref(), &context.write);
    let conversation = view
        .cat(&track, &format!("cards/{card}/conversation.md"))
        .await
        .unwrap();
    assert!(conversation.content.contains("streamed"));
}
