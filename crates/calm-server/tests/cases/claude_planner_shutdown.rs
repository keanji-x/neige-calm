//! Exercise the shutdown adapter's live and replay entries with Codex unavailable.
use super::claude_planner_stack_fixture::{Root, Stack};
use calm_server::claude_planner::stop::{
    clear_claude_planner_stop_failure_for_test, fail_claude_planner_stop_for_test,
};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::event::EventBus;
use calm_server::operation::planner_harness_shutdown_adapter::{
    PlannerHarnessShutdownAdapter, PlannerHarnessShutdownOperationPayload,
};
use calm_server::operation::{
    Operation, OperationCompletionBus, Phase, ProviderAdapter, SpawnCtx, SqlxOperationRepo,
    TxOutput,
};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

async fn shutdown(
    stack: &Stack,
    root: &Root,
    id: &str,
    unreadable: bool,
) -> calm_server::error::Result<calm_server::operation::SpawnOutcome> {
    let repo = Arc::new(SqlxRepo::open(&root.db_url()).await.unwrap());
    let wiring = stack.state.planner_wiring();
    let adapter = PlannerHarnessShutdownAdapter::new(
        stack.state.harness.clone(),
        stack.state.shared_codex_appserver.clone(),
        repo.clone(),
        wiring.claude,
        wiring.acp,
    );
    let ctx = SpawnCtx::new(
        repo.clone(),
        Arc::new(SqlxOperationRepo::new(repo.db().clone())),
        Arc::new(calm_server::state::DaemonClient::new_stub()),
        calm_server::terminal_renderer::TerminalRendererRegistry::new_with_repo(repo.clone()),
        EventBus::new(),
        OperationCompletionBus::new(),
    );
    let payload = serde_json::to_value(PlannerHarnessShutdownOperationPayload {
        worker_session_id: id.into(),
    })
    .unwrap();
    let op = Operation {
        id: "shutdown-test".into(),
        operation_key: "shutdown-test".into(),
        kind: "planner-harness-shutdown".into(),
        idempotency_key: None,
        payload_hash: String::new(),
        target_type: "runtime".into(),
        target_id: Some(id.into()),
        target: json!({}),
        payload: payload.clone(),
        tx_output: None,
        phase: Phase::TxCommitted,
        phase_detail: None,
        attempt: 1,
        last_error: None,
        compensation_state: None,
        lease_owner: None,
        lease_until_ms: None,
        spawn_artifacts: None,
        parked_at_ms: None,
        parked_deadline_ms: None,
    };
    let mut output = TxOutput::new("runtime", Some(id.into()), json!({}));
    output.data = payload;
    if unreadable {
        sqlx::query("ALTER TABLE worker_sessions RENAME TO worker_sessions_hidden")
            .execute(repo.pool())
            .await
            .unwrap();
    }
    let result = adapter.spawn_side_effect(&output, &op, &ctx).await;
    if unreadable {
        sqlx::query("ALTER TABLE worker_sessions_hidden RENAME TO worker_sessions")
            .execute(repo.pool())
            .await
            .unwrap();
    }
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn miss_stops_only_managed_claude_and_swallows_stop_failure_without_codex() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_, card) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card).await;
    stack
        .state
        .harness
        .shutdown_and_remove(&runtime.id)
        .await
        .unwrap();
    assert!(!stack.state.shared_codex_appserver.is_running());
    let repo = SqlxRepo::open(&root.db_url()).await.unwrap();
    for kind in [
        WorkerSessionKind::SharedPlanner,
        WorkerSessionKind::ManagedAssistant,
        WorkerSessionKind::ClaudeCard,
    ] {
        let id = calm_server::model::new_id();
        let mut tx = repo.pool().begin().await.unwrap();
        let row = session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: id.clone(),
                card_id: card.clone(),
                kind: kind.clone(),
                agent_provider: Some(AgentProvider::Claude),
                status: WorkerSessionState::Superseded,
                terminal_run_id: None,
                thread_id: None,
                session_id: None,
                active_turn_id: None,
                handle_state_json: None,
                spawn_op_id: None,
                now_ms: calm_server::model::now_ms(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(row.kind, kind);
        root.spawn_marked_orphan(&id);
        if kind == WorkerSessionKind::ClaudeCard {
            // Nonmanaged rows retain the Codex fallback (no thread => no RPC).
            assert!(shutdown(&stack, &root, &id, false).await.is_ok());
            assert!(
                !root.marked_pids(&id).is_empty(),
                "nonmanaged row is not marker-stopped"
            );
        } else {
            fail_claude_planner_stop_for_test(&id);
            assert!(shutdown(&stack, &root, &id, false).await.is_ok());
            assert!(
                !root.marked_pids(&id).is_empty(),
                "failed stop signals nothing"
            );
            clear_claude_planner_stop_failure_for_test(&id);
            assert!(shutdown(&stack, &root, &id, false).await.is_ok());
            assert!(
                root.wait_unmarked(&id, Duration::from_secs(5))
                    .await
                    .is_empty(),
                "managed replay must stop its original marker"
            );
        }
        assert!(
            shutdown(&stack, &root, &id, true).await.is_err(),
            "miss still propagates row read errors"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_extra_stop_uses_harness_id_even_with_unreadable_row_and_swallows_failure() {
    use calm_server::claude_planner::session::{ClaudePlannerSession, ClaudePlannerSessionParams};
    use calm_server::harness::{
        HarnessConfig, HarnessSnapshot, PlannerBackend, PlannerHarness, PlannerHarnessParams,
    };
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (track_id, card) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card).await;
    stack
        .state
        .harness
        .shutdown_and_remove(&runtime.id)
        .await
        .unwrap();
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open(&root.db_url()).await.unwrap());
    let host = stack.state.planner_wiring().claude;
    let track = repo.track_get(&track_id).await.unwrap().unwrap();
    let harness_id = format!("{}-harness", runtime.id);
    for failed in [true, false] {
        // Deliberately distinct session marker: session.shutdown cannot satisfy the assertion
        // about the operation's extra sweep of the harness id.
        let session = Arc::new(
            ClaudePlannerSession::open(ClaudePlannerSessionParams {
                host: host.clone(),
                worker_session_id: runtime.id.clone(),
                card_id: card.clone(),
                track_id: track_id.clone(),
                cwd: track.workspace.agent_cwd().into(),
                instructions: String::new(),
                calm_tools: host.calm_tools.clone(),
                proxy: vec![],
                prior_total_tokens: 0,
                repo: repo.clone(),
                seals: stack.state.thread_seals().clone(),
            })
            .await
            .unwrap(),
        );
        let handle = PlannerHarness::run(PlannerHarnessParams {
            track_delete_locks: Default::default(),
            worker_session_id: harness_id.clone(),
            track_id: track_id.clone().into(),
            card_id: card.clone().into(),
            thread_id: None,
            repo: repo.clone(),
            events: EventBus::new(),
            card_role_cache: stack.state.card_role_cache.clone(),
            track_area_cache: stack.state.track_area_cache.clone(),
            backend: PlannerBackend::claude_for_test(session),
            live_replies: stack.state.harness.live_replies().clone(),
            config: HarnessConfig::default(),
            snapshot: HarnessSnapshot::initial(0, vec![]),
        });
        stack.state.harness.insert(harness_id.clone(), handle);
        if failed {
            root.spawn_marked_orphan(&harness_id);
            fail_claude_planner_stop_for_test(&harness_id);
        }
        assert!(shutdown(&stack, &root, &harness_id, true).await.is_ok());
        assert!(stack.state.harness.get(&harness_id).is_none());
        if failed {
            assert!(!root.marked_pids(&harness_id).is_empty());
            clear_claude_planner_stop_failure_for_test(&harness_id);
        } else {
            assert!(
                root.wait_unmarked(&harness_id, Duration::from_secs(5))
                    .await
                    .is_empty(),
                "live extra sweep remains after session shutdown"
            );
        }
    }
}
