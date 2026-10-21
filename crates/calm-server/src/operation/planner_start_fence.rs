//! The one way anything submits `planner-harness-start` (#2252, #2275). A start replaces whatever
//! session the card had, so it must not interleave with another start of the same card or with
//! a send's lazy recovery: every submitter — each route, and the scheduler's child-track
//! bootstrap — holds the card's `planner_recovery_locks` guard through the start and its wait,
//! and this type is the only thing that can both hold that guard and submit.
//! `tests/cases/planner_start_fence_invariant.rs` keeps the submit here.
//!
//! Lock order (`state.rs`): the fence is `planner_recovery_locks`, so a caller may hold
//! `area_delete_locks`, `conversation_first_message_locks`, `planner_input_key_locks`, or the
//! scheduler's per-track pass lock and a spawn permit when it takes it, and never the operation
//! runtime's drive mutex or `track_delete_locks`.

use std::sync::Arc;

use crate::db::RouteRepo;
use crate::error::{CalmError, Result};
use crate::ids::CardId;
use crate::operation::planner_harness_start_adapter::PlannerHarnessStartOperationPayload;
use crate::operation::{OperationKey, OperationResult, OperationRuntime};
use crate::per_card_lock::{PerCardLockGuard, PerCardLocks, lock_card};
use crate::routes::conversations_shared::PLANNER_HARNESS_START;
use crate::session_projection_repo::CardConversation;

/// The card's `planner_recovery_locks` guard, and with it the right to start the card.
pub(crate) struct CardStartFence {
    card_id: CardId,
    repo: Arc<dyn RouteRepo>,
    operation_runtime: Arc<OperationRuntime>,
    historical_human: Option<(
        Arc<super::planner_harness_start_adapter::PlannerHarnessStartAdapter>,
        crate::conversation_inputs::HistoricalHumanSource,
    )>,
    _guard: PerCardLockGuard,
}

/// Transaction-local start inventory has no resumable reservation for this card.
/// Caller retains CardStartFence and the operation drive guard through commit.
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "transaction proof consumed by the next rotation slice"
)]
pub(crate) struct SettledCardStart {
    _card: CardId,
}

impl CardStartFence {
    /// Wait for the card's guard in `locks`, the process's one `planner_recovery_locks` map
    /// (`BootState` hands the same map to the routes and the scheduler). The only constructor.
    /// The card need not exist yet: a conversation create fences the id its operation will mint.
    pub(crate) async fn lock(
        locks: &PerCardLocks,
        repo: &Arc<dyn RouteRepo>,
        operation_runtime: &Arc<OperationRuntime>,
        card_id: &CardId,
    ) -> Self {
        let guard = lock_card(locks, card_id.as_str()).await;
        Self {
            card_id: card_id.clone(),
            repo: repo.clone(),
            operation_runtime: operation_runtime.clone(),
            historical_human: None,
            _guard: guard,
        }
    }

    /// Transient HTTP invocation context, never an authority inferred from an
    /// Actor or stored operation. Internal historical callers keep `start`.
    pub(crate) fn with_historical_human(
        mut self,
        adapter: Arc<super::planner_harness_start_adapter::PlannerHarnessStartAdapter>,
        source: crate::conversation_inputs::HistoricalHumanSource,
    ) -> Self {
        self.historical_human = Some((adapter, source));
        self
    }

    /// Retain the original card fence and request source until the owning
    /// invocation finishes. Dropping an HTTP waiter does not cancel native work.
    pub(crate) async fn wake_retained(
        self,
        adapter: Arc<super::planner_harness_start_adapter::PlannerHarnessStartAdapter>,
        source: impl Into<crate::conversation_inputs::InputSource>,
        workspace_root: std::path::PathBuf,
        events: crate::event::EventBus,
    ) -> Result<()> {
        let source = source.into();
        tokio::spawn(async move {
            let result = adapter
                .wake_retained(
                    &self.operation_runtime,
                    &self.card_id,
                    source,
                    workspace_root,
                    &events,
                )
                .await;
            drop(self);
            result
        })
        .await
        .map_err(|_| CalmError::Internal("retained activation task failed".into()))?
    }

    /// What a fresh start would lose, read under the guard, so no other start or recovery of
    /// the card can move it before this fence's own start.
    pub(crate) async fn conversation(&self) -> Result<CardConversation> {
        Ok(self
            .repo
            .session_projection_conversation_for_card(&self.card_id.to_string())
            .await?)
    }

    /// Archive admission, while the caller also holds the runtime's drive lock.
    /// The request fence prevents new submissions; the drive lock freezes preparation,
    /// provider effects and compensation. A cancelled request leaves its durable start
    /// behind, so an idle session alone does not establish that the card is settled.
    pub(crate) async fn require_settled(&self) -> Result<()> {
        // Reuse the operation owner's recovery inventory: it includes every resumable
        // start phase and compensation, regardless of lease age. Start has no explicit
        // retry phase; terminal/stuck operations cannot be driven again.
        require_no_start(
            self.operation_runtime
                .repo
                .abandoned_running_operations_on_boot()
                .await?,
            &self.card_id,
        )
    }

    #[allow(
        dead_code,
        reason = "transaction proof consumed by the next rotation slice"
    )]
    pub(crate) async fn require_settled_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        card: &str,
    ) -> Result<SettledCardStart> {
        require_no_start(
            super::repo_sqlite::abandoned_running_operations_on(tx).await?,
            &CardId::from(card),
        )?;
        Ok(SettledCardStart {
            _card: CardId::from(card),
        })
    }

    /// Submit one `planner-harness-start` for this fence's card and wait for its outcome. The
    /// caller maps the outcome; a payload naming another card is refused before anything is
    /// submitted.
    pub(crate) async fn start(
        &self,
        payload: &PlannerHarnessStartOperationPayload,
        key: OperationKey,
    ) -> Result<OperationResult> {
        self.start_wire(
            &payload.planner_card_id,
            serde_json::to_value(payload)?,
            key,
        )
        .await
    }

    pub(crate) async fn start_assistant(
        &self,
        wire: &super::assistant_start_wire::AssistantStartWire,
        key: OperationKey,
    ) -> Result<OperationResult> {
        self.start_wire(
            &wire.payload.planner_card_id,
            serde_json::to_value(wire)?,
            key,
        )
        .await
    }

    async fn start_wire(
        &self,
        card_id: &CardId,
        payload: serde_json::Value,
        key: OperationKey,
    ) -> Result<OperationResult> {
        if card_id != &self.card_id {
            return Err(CalmError::Internal(format!(
                "a planner start for card {} was submitted under card {}'s start fence",
                card_id, self.card_id
            )));
        }
        if let Some((registered, source)) = &self.historical_human {
            source
                .require_card(self.repo.as_ref(), card_id.as_str())
                .await?;
            let invocation = registered.historical_human_invocation(source.clone());
            let result = self
                .operation_runtime
                .execute_with_adapter(registered.clone(), Arc::new(invocation), key, payload)
                .await?;
            source
                .require_card(self.repo.as_ref(), card_id.as_str())
                .await?;
            return Ok(result);
        }
        let op_id = self
            .operation_runtime
            .submit(PLANNER_HARNESS_START, key, payload)
            .await?;
        self.operation_runtime.wait(&op_id).await
    }
}

/// Called only after Human acceptance/ACK commits, for both Fresh and Replayed
/// requests. The original lifecycle owner re-reads the actual current head.
pub(crate) fn wake_canonical_human(
    state: crate::state::RouteState,
    card_id: CardId,
    source: crate::auth::HumanInputSource,
) {
    tokio::spawn(async move {
        let fence = CardStartFence::lock(
            &state.planner_recovery_locks,
            &state.repo,
            &state.operation_runtime,
            &card_id,
        )
        .await;
        if let Err(error) = fence
            .wake_retained(
                state.planner_start_adapter.clone(),
                source,
                state.workspace_root.clone(),
                state.events.clone(),
            )
            .await
        {
            let reason = match error {
                CalmError::Unauthorized | CalmError::Forbidden(_) => "source_refused",
                CalmError::Conflict(_) => "retained_state_held",
                _ => "activation_unavailable",
            };
            tracing::warn!(%card_id, reason, "accepted canonical work remains held by its original lifecycle");
        }
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("canonical-human-wake-finished", card_id.as_str()).await;
    });
}

fn require_no_start(operations: Vec<super::Operation>, card: &CardId) -> Result<()> {
    for op in operations {
        if op.kind == PLANNER_HARNESS_START
            && super::planner_harness_start_adapter::operation_card_id(&op.payload)? == *card
        {
            return Err(CalmError::Conflict(
                "Wait for the conversation session start to finish before archiving.".into(),
            ));
        }
    }
    Ok(())
}
