use async_trait::async_trait;
use calm_types::model::TrackLifecycle;
use calm_types::worker::{Liveness, WorkerSession, WorkerSessionId, WorkerSessionState};
use sqlx::{Sqlite, Transaction};

use crate::error::Result;
use crate::ids::{AreaId, TrackId};

pub type Tx<'a> = Transaction<'a, Sqlite>;

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum CommitExitOutcome {
    Committed(WorkerSession),
    Absorbed,
}

/// A track whose root the reaper's scan found POSITIVELY dead, eligible for
/// `Draft|Planning → Failed` convergence. Computed entirely in SQL; a live or
/// merely just-created track is never a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadRootCandidate {
    pub track_id: TrackId,
    pub area_id: AreaId,
    /// Always `Draft` (failed-start) or `Planning` (lost-root); the reaper treats
    /// a current != from read as a race-loss.
    pub lifecycle: TrackLifecycle,
}

#[async_trait]
pub trait SessionRepo: Send + Sync {
    async fn session_insert_tx(
        &self,
        tx: &mut Tx<'_>,
        session: WorkerSession,
    ) -> Result<WorkerSession>;

    async fn session_get(&self, id: &WorkerSessionId) -> Result<Option<WorkerSession>>;

    async fn sessions_nonterminal(&self) -> Result<Vec<WorkerSession>>;

    async fn session_set_liveness(
        &self,
        id: &WorkerSessionId,
        liveness: &Liveness,
        probed_at_ms: i64,
    ) -> Result<Option<WorkerSession>>;

    /// Stamps the push-fed liveness columns on an *active* session without
    /// touching `updated_at_ms`. Benign no-op on a terminal/missing row.
    async fn session_record_activity(
        &self,
        id: &WorkerSessionId,
        last_activity_ms: i64,
        last_thread_status: &str,
    ) -> Result<()>;

    /// Keyed by codex `thread_id` (the notification subscriber only sees thread
    /// ids); pinned to `provider='codex'`, never touches `updated_at_ms`.
    /// `turn_completed_ms` is `Some` only for a `turn/completed` with
    /// `status = completed` and raises `last_turn_completed_ms` monotonically.
    async fn session_record_activity_by_thread(
        &self,
        thread_id: &str,
        last_activity_ms: i64,
        last_thread_status: &str,
        turn_completed_ms: Option<i64>,
    ) -> Result<()>;

    async fn session_state_transition_tx(
        &self,
        tx: &mut Tx<'_>,
        id: &WorkerSessionId,
        to: WorkerSessionState,
    ) -> Result<WorkerSession>;

    async fn session_commit_exit(
        &self,
        id: &WorkerSessionId,
        to: WorkerSessionState,
        liveness_probed_at_ms: i64,
        exit_code: Option<i32>,
        exit_interpretation: &str,
    ) -> Result<CommitExitOutcome>;

    async fn session_list_by_track(&self, track_id: &TrackId) -> Result<Vec<WorkerSession>>;

    /// Tracks whose ROOT is POSITIVELY dead (`Draft` with a failed start-op,
    /// `Planning` with a NULL/terminal root), and with NO active planner-contract
    /// session — never a live or just-created track. Boot-gating is the caller's.
    async fn dead_root_candidates(&self) -> Result<Vec<DeadRootCandidate>>;
}
