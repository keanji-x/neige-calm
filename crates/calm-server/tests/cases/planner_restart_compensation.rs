//! #2192 — a fresh start over a card's failed carrier that then fails gives the card back to that
//! carrier, owing exactly what the new session did not deliver: the queue it kept, attachments
//! included, minus anything the new session already handed to the model.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::http::StatusCode;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::error::{CalmError, Result as CalmResult};
use calm_server::harness::{HarnessSnapshot, HarnessState, is_harness_snapshot_value};
use calm_server::model::new_id;
use calm_server::operation::planner_harness_start_adapter::PlannerHarnessStartAdapter;
use calm_server::operation::{
    AppServerInteractKind, AppServerInteractOutcome, CompensationStateVersioned, CompensationStep,
    Operation, OperationCompletionBus, OperationRuntime, PhaseTag, ProviderAdapter, SpawnCtx,
    SpawnOutcome, SqlxOperationRepo, Tx, TxOutput,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use serde_json::{Value, json};

use crate::planner_first_start::{Boot, post_input};
use crate::planner_restart::{bound_image, get, ordinary_track, wait_for};

/// The production start adapter, except that the spawn reports a failure AFTER the real spawn
/// installed the new session and that session handed `delivered` to the model and dropped it from
/// its row: the shape of a final-persistence error after install, or of a crash whose recovery
/// reinstalls the new session before the compensation resumes. Everything else, the compensation
/// included, is the production adapter's own.
struct DeliverThenFailSpawn {
    inner: PlannerHarnessStartAdapter,
    repo: Arc<SqlxRepo>,
    daemon: Arc<SharedCodexAppServer>,
    card_id: String,
    delivered: &'static str,
}

impl DeliverThenFailSpawn {
    async fn new_session_still_holds(&self) -> bool {
        let rows: Vec<Option<String>> = sqlx::query_scalar(
            "SELECT handle_state_json FROM worker_sessions WHERE card_id = ?1 \
               AND state IN ('starting','running','idle','turn_pending')",
        )
        .bind(&self.card_id)
        .fetch_all(self.repo.pool())
        .await
        .unwrap();
        rows.into_iter().flatten().any(|state| {
            let value: Value = serde_json::from_str(&state).unwrap();
            is_harness_snapshot_value(&value)
                && format!(
                    "{:?}",
                    HarnessSnapshot::from_value_strict(value).pending_entries()
                )
                .contains(self.delivered)
        })
    }
}

#[async_trait]
impl ProviderAdapter for DeliverThenFailSpawn {
    fn kind(&self) -> &'static str {
        self.inner.kind()
    }

    fn phases(&self) -> &'static [PhaseTag] {
        self.inner.phases()
    }

    fn app_server_interact_kind(
        &self,
        output: &TxOutput,
        op: &Operation,
    ) -> CalmResult<AppServerInteractKind> {
        self.inner.app_server_interact_kind(output, op)
    }

    async fn validate(&self, input: &Value) -> CalmResult<()> {
        self.inner.validate(input).await
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        op: &Operation,
    ) -> CalmResult<TxOutput> {
        self.inner.prepare_tx(tx, input, op).await
    }

    async fn app_server_interact(
        &self,
        output: &mut TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> CalmResult<AppServerInteractOutcome> {
        self.inner.app_server_interact(output, op, ctx).await
    }

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> CalmResult<SpawnOutcome> {
        self.inner.spawn_side_effect(output, op, ctx).await?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let handed_over =
                format!("{:?}", self.daemon.started_turns_for_test()).contains(self.delivered);
            if handed_over && !self.new_session_still_holds().await {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "premise: the new session never delivered the carried message"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Err(CalmError::Internal(
            "test: the start failed after its new session delivered the carried message".into(),
        ))
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        op: &Operation,
    ) -> CalmResult<CompensationStateVersioned> {
        self.inner
            .plan_compensation(from_phase, reason, output, op)
            .await
    }

    async fn compensate_step(
        &self,
        step: &CompensationStep,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> CalmResult<()> {
        self.inner.compensate_step(step, output, op, ctx).await
    }
}

/// Swap the operation runtime for one whose start adapter is [`DeliverThenFailSpawn`]; returns
/// the boot's own state, to put back.
fn fail_starts_after_delivering(
    boot: &mut Boot,
    delivered: &'static str,
) -> calm_server::state::AppState {
    let original = boot.state.clone();
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = boot.repo.clone();
    let operation_repo = Arc::new(SqlxOperationRepo::new(boot.repo.pool().clone()));
    let adapter: Arc<dyn ProviderAdapter> = Arc::new(DeliverThenFailSpawn {
        inner: PlannerHarnessStartAdapter::new(
            boot.repo.clone(),
            boot.state.shared_codex_appserver.clone(),
            boot.state.thread_seals().clone(),
            boot.state.harness.clone(),
            boot.state.plugin.clone(),
            boot.state.card_role_cache.clone(),
            boot.state.track_area_cache.clone(),
            None,
            boot.state.claude_planner_wiring().host,
        ),
        repo: boot.repo.clone(),
        daemon: boot.state.shared_codex_appserver.clone(),
        card_id: boot.planner_card_id.clone(),
        delivered,
    });
    let completion = OperationCompletionBus::new();
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo.clone(),
        vec![adapter],
        boot.state.events.clone(),
        completion.clone(),
        SpawnCtx::new(
            route_repo,
            operation_repo,
            boot.state.daemon.clone(),
            boot.state.terminal_renderer.clone(),
            boot.state.events.clone(),
            completion,
        ),
    ));
    boot.state = boot.state.clone().with_operation_runtime(runtime);
    boot.app = crate::planner_first_start::router(&boot.state);
    original
}

/// A restart whose `thread/start` fails gives the card back to the wedged session, with the queue
/// still owed there; the next restart delivers it once, image included.
#[tokio::test]
async fn a_failed_restart_gives_the_card_back_to_its_wedged_session() {
    const QUEUED: &str = "the message a failed restart must not strand";
    let boot = ordinary_track().await;
    let image = bound_image(&boot);
    let wedged = boot
        .wedge_with_queued(QUEUED, "interrupt_timeout", &image)
        .await;

    boot.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (status, body) = boot.fresh_start("restart").await;
    assert!(
        !status.is_success(),
        "premise: the start failed: {status} body={body}"
    );

    assert_eq!(
        boot.current_session().await,
        Some(wedged.clone()),
        "the card's session is the wedged one again"
    );
    assert_eq!(
        boot.harvested_at(&wedged).await,
        None,
        "and it owes its queue again"
    );
    let (status, run) = get(boot.app.clone(), boot.card_uri("planner/run")).await;
    assert_eq!(status, StatusCode::OK, "body={run}");
    assert_eq!(run["phase"], "wedged", "body={run}");
    assert_eq!(run["pending"][0]["text"], QUEUED, "body={run}");
    assert_eq!(
        run["pending"][0]["attachments"].as_array().map(Vec::len),
        Some(1),
        "body={run}"
    );
    assert_eq!(
        boot.queued_copies(QUEUED).await,
        vec![(wedged.clone(), 1)],
        "the failed start's own row owes nothing"
    );

    let (status, body) = boot.fresh_start("restart").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    wait_for("the message reaches the model", async || {
        !boot.deliveries(QUEUED, &image.path).is_empty()
            && boot.queued_copies(QUEUED).await.is_empty()
    })
    .await;
    assert_eq!(boot.deliveries(QUEUED, &image.path), vec![true]);
    boot.shutdown().await;
}

/// A failed restart over a system-error session leaves it recoverable: a person's send resumes it,
/// and the queued message and its image go out with it.
#[tokio::test]
async fn a_failed_restart_leaves_a_system_error_session_recoverable_by_a_send() {
    const QUEUED: &str = "the message queued before the provider error";
    let boot = ordinary_track().await;
    let image = bound_image(&boot);
    let failed = boot.wedge_with_queued(QUEUED, "system_error", &image).await;

    boot.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (status, body) = boot.fresh_start("restart").await;
    assert!(
        !status.is_success(),
        "premise: the start failed: {status} body={body}"
    );
    assert_eq!(boot.current_session().await, Some(failed.clone()));

    let (status, body) =
        post_input(boot.app.clone(), &boot.input_uri(), "carry on", &new_id()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(
        body["worker_session_id"],
        json!(failed),
        "the send recovered the original session"
    );
    wait_for("the queued message reaches the model", async || {
        !boot.deliveries(QUEUED, &image.path).is_empty()
    })
    .await;
    assert_eq!(boot.deliveries(QUEUED, &image.path), vec![true]);
    boot.shutdown().await;
}

/// The same with nothing queued: no give-back touches the carrier, so the restore itself must
/// leave it unstamped, or the system-error recovery refuses it.
#[tokio::test]
async fn a_failed_restart_leaves_an_empty_system_error_session_recoverable_by_a_send() {
    let boot = ordinary_track().await;
    let failed = boot.active().await.id;
    let harness = boot.state.harness.get(&failed).unwrap();
    harness
        .set_state_for_test(HarnessState::Wedged {
            since: Instant::now(),
            reason: "system_error".into(),
        })
        .await;
    harness.persist_snapshot().await.unwrap();
    let (status, run) = get(boot.app.clone(), boot.card_uri("planner/run")).await;
    assert_eq!(status, StatusCode::OK, "body={run}");
    assert_eq!(
        run["pending"],
        json!([]),
        "premise: nothing is queued: {run}"
    );

    boot.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (status, body) = boot.fresh_start("restart").await;
    assert!(
        !status.is_success(),
        "premise: the start failed: {status} body={body}"
    );
    assert_eq!(boot.current_session().await, Some(failed.clone()));
    assert_eq!(boot.harvested_at(&failed).await, None);

    let (status, body) =
        post_input(boot.app.clone(), &boot.input_uri(), "carry on", &new_id()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(
        body["worker_session_id"],
        json!(failed),
        "the send recovered the original session"
    );
    boot.shutdown().await;
}

/// A start that fails AFTER its new session delivered the carried message gives the card back
/// to the wedged session without that message: it was sent, so nothing owes it.
#[tokio::test]
async fn a_failed_start_does_not_give_back_what_its_new_session_delivered() {
    const QUEUED: &str = "the message the new session already sent";
    let mut boot = ordinary_track().await;
    let image = bound_image(&boot);
    let wedged = boot
        .wedge_with_queued(QUEUED, "interrupt_timeout", &image)
        .await;

    let original = fail_starts_after_delivering(&mut boot, QUEUED);
    let (status, body) = boot.fresh_start("restart").await;
    assert!(
        !status.is_success(),
        "premise: the start failed: {status} body={body}"
    );
    assert_eq!(
        boot.deliveries(QUEUED, &image.path),
        vec![true],
        "premise: the new session delivered it"
    );
    assert_eq!(
        boot.current_session().await,
        Some(wedged.clone()),
        "premise: the card is given back"
    );

    let (status, run) = get(boot.app.clone(), boot.card_uri("planner/run")).await;
    assert_eq!(status, StatusCode::OK, "body={run}");
    assert_eq!(
        run["pending"],
        json!([]),
        "the delivered message is not pending again: body={run}"
    );
    assert_eq!(
        boot.queued_copies(QUEUED).await,
        Vec::<(String, usize)>::new(),
        "no row owes it"
    );

    boot.state = original;
    boot.app = crate::planner_first_start::router(&boot.state);
    let (status, body) = boot.fresh_start("restart").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(
        boot.queued_copies(QUEUED).await,
        Vec::<(String, usize)>::new(),
        "the next restart carries nothing"
    );
    assert_eq!(
        boot.deliveries(QUEUED, &image.path),
        vec![true],
        "delivered once"
    );
    boot.shutdown().await;
}
