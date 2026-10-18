//! Shared setup for the actual activation operation and its real snapshot owner.
use super::completed_commit_tests::Fixture;
use super::*;
use crate::db::prelude::*;
use crate::harness::HarnessRegistry;
use crate::operation::handoff_activation::{self, ActivationResult};
use crate::operation::{OperationReceipt, OperationRuntime};

pub(super) fn runtime(a: &Fixture) -> (OperationRuntime, HarnessRegistry) {
    let registry = HarnessRegistry::new();
    (
        runtime_with_repo(a, a.repo.clone(), registry.clone()),
        registry,
    )
}

pub(super) fn runtime_with_repo(
    a: &Fixture,
    repo: Arc<dyn crate::db::Repo>,
    registry: HarnessRegistry,
) -> OperationRuntime {
    let wiring =
        crate::claude_planner::wiring::ClaudePlannerWiring::unconfigured_for_test(repo.clone());
    let start = Arc::new(
        crate::operation::planner_harness_start_adapter::PlannerHarnessStartAdapter::new(
            repo.clone(),
            a.daemon.clone(),
            a.daemon.thread_seals().clone(),
            registry.clone(),
            wiring.plugin,
            a.harness.inner.card_role_cache.clone(),
            a.harness.inner.track_area_cache.clone(),
            Some("/tmp/activation-stub.sock".into()),
            wiring.claude,
            wiring.acp,
        ),
    );
    let operations: Arc<dyn crate::operation::OperationRepo> = Arc::new(
        crate::operation::SqlxOperationRepo::new(repo.sqlite_pool().unwrap()),
    );
    let events = EventBus::new();
    let completion = crate::operation::OperationCompletionBus::new();
    let ctx = crate::operation::SpawnCtx::new(
        repo,
        operations.clone(),
        Arc::new(crate::state::DaemonClient::new_stub()),
        crate::terminal_renderer::TerminalRendererRegistry::new(),
        events.clone(),
        completion.clone(),
    );
    OperationRuntime::new_unchecked(
        operations,
        vec![Arc::new(handoff_activation::HandoffActivationAdapter {
            start,
        })],
        events,
        completion,
        ctx,
    )
}

pub(super) fn original(result: crate::operation::handoff_cutover::CutoverResult) -> String {
    let crate::operation::handoff_cutover::CutoverResult::CutoverCommittedPendingActivation {
        operation_id,
        ..
    } = result;
    operation_id
}

pub(super) fn result(receipt: &OperationReceipt) -> ActivationResult {
    let value = match receipt {
        OperationReceipt::AwaitingRetry { output, .. } => output.result.clone(),
        OperationReceipt::Settled(crate::operation::OperationResult {
            outcome: crate::operation::OperationOutcome::Succeeded { result },
            ..
        }) => result.clone(),
        other => panic!("expected durable activation result, got {other:?}"),
    };
    serde_json::from_value(value).unwrap()
}

pub(super) async fn stored(a: &Fixture, session: &str) -> HarnessSnapshot {
    let row = a
        .repo
        .session_get_by_id(&session.into())
        .await
        .unwrap()
        .unwrap();
    HarnessSnapshot::parse_known(row.handle_state_json.unwrap()).unwrap()
}

pub(super) async fn wait_for_resource_intent(a: &Fixture) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let count: i64 = sqlx::query_scalar(concat!("SELECT count(*) FROM operations WHERE kind='handoff-activation' AND json_extract(tx_output_json,","'$.result.state')='resource_request_recorded'"))
                .fetch_one(a.repo.pool()).await.unwrap();
            if count == 1 { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("real operation must persist resource intent before the native RPC");
}
