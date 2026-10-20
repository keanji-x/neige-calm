//! #2774: a new, blank Planner conversation on the same card. The restart's own `prepare_tx`
//! transaction is the cutover; this owner adds the compare-and-set on the session the person's
//! request was admitted against, the conversation floor, and the one card announcement.
use super::*;
use calm_types::worker::WorkerSessionId;

/// Present only on the start a new conversation submits; `None` is skipped by serde, so every
/// other start keeps its byte-identical payload and hash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewConversation {
    /// The card's current session when the request was admitted. The cutover commits only while
    /// it still is, active and still holding its queue.
    pub expected_session_id: String,
}

fn changed() -> CalmError {
    CalmError::Conflict(
        "The Planner conversation changed while the new one was being started; try again.".into(),
    )
}

/// The refusals admission (the route) and the cutover (`prepare_tx`) share, so both say the same.
pub(crate) fn busy() -> CalmError {
    CalmError::Conflict(
        "Wait for the Planner's reply and queued messages to finish before starting a new conversation."
            .into(),
    )
}

pub(crate) fn unconfirmed() -> CalmError {
    CalmError::Conflict(
        "The Planner's state is not confirmed; restart it before starting a new conversation."
            .into(),
    )
}

pub(crate) fn hold_open() -> CalmError {
    CalmError::Conflict(
        "Answer the Planner's open question before starting a new conversation.".into(),
    )
}

/// Only a person's restart-shaped Planner start may cut a card over.
pub(super) fn validate(payload: &PlannerHarnessStartOperationPayload) -> Result<()> {
    if payload.new_conversation.is_none() {
        return Ok(());
    }
    if payload.actor != ActorId::User {
        return Err(CalmError::Forbidden(
            "Only the person starts a new Planner conversation".into(),
        ));
    }
    if payload.profile != HarnessProfile::Planner
        || !payload.force_new_thread
        || payload.reset_harness_items
        || payload.goal.is_some()
        || payload.create_card.is_some()
        || payload.first_message.is_some()
        || payload.opening_briefing.is_some()
    {
        return Err(CalmError::BadRequest(
            "a new conversation is a blank Planner restart on a new thread".into(),
        ));
    }
    Ok(())
}

impl PlannerHarnessStartAdapter {
    /// This adapter for one person's request: the cutover commits only under that person's
    /// current access. Never persisted, so a start driven without the request commits nothing.
    pub(crate) fn new_conversation_invocation(&self, requester: crate::auth::Principal) -> Self {
        let mut invocation = self.clone();
        invocation.invocation = StartInvocation::NewConversation(requester);
        invocation
    }

    /// The compare-and-set and the requester's access, read in the cutover's transaction before
    /// its first write: the card's session is still the expected one, active, unharvested, not
    /// paused on a `hold` ask, and its persisted queue holds no sentence a person typed; and the
    /// person who asked may still work in the Track, as archive rechecks it where it commits.
    pub(super) async fn require_new_conversation_tx(
        &self,
        tx: &mut Tx<'_>,
        card: &Card,
        expected: &NewConversation,
        active: Option<&crate::session_projection_repo::WorkerSessionProjection>,
    ) -> Result<()> {
        require_expected_tx(tx, card, expected, active).await?;
        let StartInvocation::NewConversation(requester) = &self.invocation else {
            return Err(CalmError::Forbidden(
                "A new Planner conversation starts only for the request of the person who asked; ask again."
                    .into(),
            ));
        };
        crate::track_access::require_detail_access_tx(tx, card.track_id.as_str(), requester)
            .await?;
        Ok(())
    }
}

async fn require_expected_tx(
    tx: &mut Tx<'_>,
    card: &Card,
    expected: &NewConversation,
    active: Option<&crate::session_projection_repo::WorkerSessionProjection>,
) -> Result<()> {
    let unharvested: Option<bool> = sqlx::query_scalar(
        "SELECT ws.queue_harvested_at_ms IS NULL FROM cards c \
           JOIN worker_sessions ws ON ws.id = c.session_id AND ws.card_id = c.id \
          WHERE c.id = ?1 AND ws.id = ?2 \
            AND ws.state IN ('starting', 'running', 'idle', 'turn_pending')",
    )
    .bind(card.id.as_str())
    .bind(&expected.expected_session_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(active) = active.filter(|active| active.id == expected.expected_session_id) else {
        return Err(changed());
    };
    if unharvested != Some(true) {
        return Err(changed());
    }
    if crate::ask::has_open_hold_ask(
        &mut **tx,
        &card.track_id,
        &WorkerSessionId::from(active.id.as_str()),
    )
    .await?
    {
        return Err(hold_open());
    }
    let snapshot = active
        .handle_state_json
        .clone()
        .and_then(HarnessSnapshot::parse_known)
        .ok_or_else(unconfirmed)?;
    refuse_user_entries(&snapshot.pending_entries())
}

/// System observations move to the new conversation; a sentence a person typed never does.
pub(crate) fn refuse_user_entries(entries: &[QueueEntry]) -> Result<()> {
    if entries.iter().any(QueueEntry::is_user_authored) {
        return Err(busy());
    }
    Ok(())
}

/// Stranded sentences the harvest found on retired rows of the card would open the blank
/// conversation with a person's old words.
pub(super) fn refuse_harvested(messages: &[HarvestedMessage]) -> Result<()> {
    if !messages.is_empty() {
        return Err(busy());
    }
    Ok(())
}

/// Write the conversation floor at the card's newest transcript row and announce the card, in
/// the cutover's transaction. Returns the new conversation's ordinal.
pub(super) async fn record_floor_tx(
    tx: &mut Tx<'_>,
    actor: &ActorId,
    card: &Card,
    now: i64,
    post_commit_events: &mut Vec<BroadcastEnvelope>,
) -> Result<i64> {
    let ordinal =
        crate::db::sqlite::planner_conversation_start_tx(tx, card.id.as_str(), now).await?;
    let updated = card_update_tx(tx, card.id.as_str(), CardPatch::default()).await?;
    // `card_scope_tx`: the session writes above hold this transaction's lock on `tracks`.
    let scope = card_scope_tx(tx, card.id.clone(), card.track_id.clone()).await?;
    let event = Event::CardUpdated(updated);
    let id = append_decision_event_in_tx(tx, actor, &scope, None, &event).await?;
    post_commit_events.push(BroadcastEnvelope {
        id,
        event_version: SYNC_EVENT_VERSION,
        actor: actor.clone(),
        scope,
        event,
    });
    Ok(ordinal)
}

/// A committed cutover is final: a failed start keeps the new session as a failed carrier of the
/// moved queue, and the superseded session is never given the card back.
pub(super) fn restores_predecessor(payload: &PlannerHarnessStartOperationPayload) -> bool {
    payload.new_conversation.is_none()
}

/// `snapshot` is the new row's own persisted queue and watermark (an ACP provider's full
/// checkpoint, or the queue and watermark adopted together from the row), so the new session
/// catches up on the events above it as a recovered Planner does, whatever its provider. An
/// event committed while the new row was `starting` and no request held the card (a boot or lease
/// re-drive of this start) found no harness to push to, and nothing else would deliver it; one
/// the dispatcher still holds behind the request's fence, or one an earlier drive of this start
/// already caught up on, is deduplicated against this watermark.
pub(super) async fn catch_up(
    repo: Arc<dyn Repo>,
    payload: &PlannerHarnessStartOperationPayload,
    card_id: &str,
    track_id: &str,
    snapshot: &mut HarnessSnapshot,
) -> Result<()> {
    if payload.new_conversation.is_none() {
        return Ok(());
    }
    let watermark = snapshot.push_watermark;
    crate::harness::replay_harness_events_since(
        repo,
        card_id,
        &TrackId::from(track_id.to_string()),
        watermark,
        snapshot,
    )
    .await
}
