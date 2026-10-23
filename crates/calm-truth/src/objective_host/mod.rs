//! Unexposed objective persistence. Only a committed reaction may be driven.
//!
//! The engine owns semantics; this module owns authorization, atomic storage and
//! source idempotency. Replay rebuilds state, never durable effects.
mod approval;
mod authorize;
pub mod capability;
mod intents;
mod lifecycle;
mod log;
pub mod report;
mod snapshot;
mod usage;
pub use usage::{TurnUsage, TurnUsageResult, read_state, record_turn_usage};

use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, Transaction};

use crate::db::sqlite::{SqliteDb, begin_immediate_tx};
use crate::error::{Result, TruthError};
use crate::ids::TrackId;
use crate::worker::{Principal, WorkerSessionId};
use objective::{Event, Intent, State};

pub use intents::{IntentId, StoredEffect, StoredIntent};
pub use lifecycle::{
    ExecutorSettlement, IntentSettlement, IntentStatus, intent_mark_dispatched_tx,
    intent_settle_tx, unsettled_intents,
};
pub use log::LoggedEvent;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogSeq(pub u64);

/// A planner tool-call id, scoped to its session by the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PlannerRequestId(String);
impl PlannerRequestId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum InputSource {
    AskAnswered {
        ask_id: i64,
    },
    ReportBlock {
        report_card_id: crate::ids::CardId,
        doc_rev: u64,
        block_id: String,
    },
    IntentJob {
        intent: IntentId,
    },
    Planner {
        principal: Principal,
        request_id: PlannerRequestId,
    },
    HarnessTurn {
        session_id: WorkerSessionId,
        turn_id: String,
    },
}

impl InputSource {
    fn identity(&self) -> Result<(&'static str, String)> {
        let (kind, session, id) = match self {
            Self::AskAnswered { ask_id } => {
                return Ok(("ask_answered", ask_id.to_string()));
            }
            Self::ReportBlock {
                report_card_id,
                doc_rev,
                block_id,
            } => {
                if report_card_id.as_str().is_empty() || block_id.is_empty() {
                    return Err(TruthError::BadRequest(
                        "objective report source identity is empty",
                    ));
                }
                return Ok((
                    "report_block",
                    serde_json::to_string(&(report_card_id, doc_rev, block_id))?,
                ));
            }
            Self::IntentJob { intent } => {
                return Ok(("intent_job", serde_json::to_string(intent)?));
            }
            Self::Planner {
                principal: Principal::Agent { session_id, .. },
                request_id,
            } => ("planner_call", session_id, &request_id.0),
            Self::Planner { .. } => {
                return Err(TruthError::Forbidden(
                    "objective planner source needs an agent".into(),
                ));
            }
            Self::HarnessTurn {
                session_id,
                turn_id,
            } => ("harness_turn", session_id, turn_id),
        };
        if session.as_str().is_empty() || id.is_empty() {
            return Err(TruthError::BadRequest("objective source identity is empty"));
        }
        Ok((kind, serde_json::to_string(&(session, id))?))
    }
}

#[derive(Clone, Debug)]
pub struct ObjectiveInput {
    pub track_id: TrackId,
    pub event: Event,
    pub source: InputSource,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Reaction {
    Appended {
        seq: LogSeq,
        intents: Vec<StoredIntent>,
    },
    Duplicate {
        seq: LogSeq,
    },
}

/// Constructed only after COMMIT succeeds; react_tx's Reaction is not this ticket.
#[derive(Debug)]
pub struct CommittedReaction {
    reaction: Reaction,
}
impl CommittedReaction {
    pub fn reaction(&self) -> &Reaction {
        &self.reaction
    }
    pub fn into_reaction(self) -> Reaction {
        self.reaction
    }
}

pub async fn react(db: &SqliteDb, input: ObjectiveInput) -> Result<CommittedReaction> {
    let mut tx = begin_immediate_tx(db).await?;
    let at_ms = now_ms()?;
    let reaction = react_tx(&mut tx, &input, at_ms).await?;
    tx.commit().await?;
    Ok(CommittedReaction { reaction })
}

fn now_ms() -> Result<i64> {
    let at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| TruthError::Internal(e.to_string()))?
        .as_millis();
    i64::try_from(at_ms).map_err(internal)
}

/// Caller owns BEGIN IMMEDIATE and must roll back on error, as for other tx APIs.
pub async fn react_tx(
    tx: &mut Transaction<'_, Sqlite>,
    input: &ObjectiveInput,
    at_ms: i64,
) -> Result<Reaction> {
    let authorized = authorize::authorize(tx, input).await?;
    apply_tx(tx, authorized, at_ms).await
}

async fn apply_tx(
    tx: &mut Transaction<'_, Sqlite>,
    authorized: authorize::AuthorizedInput<'_>,
    at_ms: i64,
) -> Result<Reaction> {
    let input = authorized.input();
    if let Some(seq) = log::dedupe_tx(tx, &authorized).await? {
        return Ok(Reaction::Duplicate { seq });
    }
    lifecycle::check_job_tx(tx, input).await?;
    let (previous, state) = snapshot::load_tx(tx, &input.track_id).await?;
    let (next, intents) = objective::react(&state, &input.event);
    let seq = LogSeq(
        previous
            .0
            .checked_add(1)
            .ok_or_else(|| TruthError::Internal("objective sequence overflow".into()))?,
    );
    log::append_tx(tx, &authorized, seq, at_ms).await?;
    snapshot::store_tx(tx, &input.track_id, previous, seq, &next).await?;
    let intents = intents::insert_tx(tx, &authorized, &state, seq, intents, at_ms).await?;
    lifecycle::report_tx(tx, input, seq, at_ms).await?;
    Ok(Reaction::Appended { seq, intents })
}

/// A version-mismatched snapshot is rebuilt in memory. This read never inserts intents.
/// Use writer admission for a stable multi-query view without a deferred-lock cycle.
pub async fn load_state(db: &SqliteDb, track: &TrackId) -> Result<(LogSeq, State)> {
    let mut tx = begin_immediate_tx(db).await?;
    let result = load_state_tx(&mut tx, track).await?;
    tx.commit().await?;
    Ok(result)
}

/// Read the engine state under the caller's writer admission, so an executor
/// can validate a notice and settle it atomically with respect to Report edits.
pub async fn load_state_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
) -> Result<(LogSeq, State)> {
    snapshot::load_tx(tx, track).await
}

/// Audit replay returns deterministic intent positions, without executing or inserting them.
pub async fn replay_log(
    db: &SqliteDb,
    track: &TrackId,
) -> Result<(State, Vec<(IntentId, Intent)>)> {
    let mut tx = begin_immediate_tx(db).await?;
    let rows = log::read_tx(&mut tx, track).await?;
    let result = fold_log(&rows);
    tx.commit().await?;
    Ok(result)
}

fn fold_log(rows: &[LoggedEvent]) -> (State, Vec<(IntentId, Intent)>) {
    let mut state = State::default();
    let mut effects = vec![];
    for row in rows {
        let (next, intents) = objective::react(&state, &row.event);
        state = next;
        for (ordinal, intent) in intents.into_iter().enumerate() {
            effects.push((
                IntentId {
                    track_id: row.track_id.clone(),
                    seq: row.seq,
                    ordinal: ordinal as u32,
                },
                intent,
            ));
        }
    }
    (state, effects)
}

#[cfg(test)]
async fn react_fixture_tx(
    tx: &mut Transaction<'_, Sqlite>,
    input: &ObjectiveInput,
    at_ms: i64,
) -> Result<Reaction> {
    apply_tx(tx, authorize::fixture(input), at_ms).await
}

fn sql_seq(seq: LogSeq) -> Result<i64> {
    i64::try_from(seq.0)
        .map_err(|_| TruthError::Internal("objective sequence exceeds SQLite integer".into()))
}
fn internal(error: impl std::fmt::Display) -> TruthError {
    TruthError::Internal(format!("objective storage: {error}"))
}

#[cfg(test)]
mod tests;
