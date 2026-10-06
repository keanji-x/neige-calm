//! The one way a route submits `planner-harness-start` (#2252). A start replaces whatever
//! session the card had, so it must not interleave with another start of the same card or with
//! a send's lazy recovery: every submitter holds the card's `planner_recovery_locks` guard
//! through the start and its wait, and this type is the only thing that can both hold that
//! guard and submit. `tests/cases/planner_start_fence_invariant.rs` keeps the submit here.
//!
//! Lock order (`state.rs`): the fence is `planner_recovery_locks`, so a caller may hold
//! `area_delete_locks`, `conversation_first_message_locks` or `planner_input_key_locks` when it
//! takes it, and never the operation runtime's drive mutex or `track_delete_locks`.

use std::sync::Arc;

use crate::db::RouteRepo;
use crate::error::{CalmError, Result};
use crate::ids::CardId;
use crate::operation::planner_harness_start_adapter::PlannerHarnessStartOperationPayload;
use crate::operation::{OperationKey, OperationResult, OperationRuntime};
use crate::per_card_lock::{PerCardLockGuard, lock_card};
use crate::routes::conversations_shared::PLANNER_HARNESS_START;
use crate::session_projection_repo::CardConversation;
use crate::state::RouteState;

/// The card's `planner_recovery_locks` guard, and with it the right to start the card.
pub(crate) struct CardStartFence {
    card_id: CardId,
    repo: Arc<dyn RouteRepo>,
    operation_runtime: Arc<OperationRuntime>,
    _guard: PerCardLockGuard,
}

impl CardStartFence {
    /// Wait for the card's `planner_recovery_locks` guard. The only constructor. The card need
    /// not exist yet: a conversation create fences the id its operation will mint.
    pub(crate) async fn lock(s: &RouteState, card_id: &CardId) -> Self {
        let guard = lock_card(&s.planner_recovery_locks, card_id.as_str()).await;
        Self {
            card_id: card_id.clone(),
            repo: s.repo.clone(),
            operation_runtime: s.operation_runtime.clone(),
            _guard: guard,
        }
    }

    /// What a fresh start would lose, read under the guard, so no other start or recovery of
    /// the card can move it before this fence's own start.
    pub(crate) async fn conversation(&self) -> Result<CardConversation> {
        Ok(self
            .repo
            .session_projection_conversation_for_card(&self.card_id.to_string())
            .await?)
    }

    /// Submit one `planner-harness-start` for this fence's card and wait for its outcome. The
    /// caller maps the outcome; a payload naming another card is refused before anything is
    /// submitted.
    pub(crate) async fn start(
        &self,
        payload: &PlannerHarnessStartOperationPayload,
        key: OperationKey,
    ) -> Result<OperationResult> {
        if payload.planner_card_id != self.card_id {
            return Err(CalmError::Internal(format!(
                "a planner start for card {} was submitted under card {}'s start fence",
                payload.planner_card_id, self.card_id
            )));
        }
        let payload = serde_json::to_value(payload)?;
        let op_id = self
            .operation_runtime
            .submit(PLANNER_HARNESS_START, key, payload)
            .await?;
        self.operation_runtime.wait(&op_id).await
    }
}
