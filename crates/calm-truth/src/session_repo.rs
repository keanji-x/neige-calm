use async_trait::async_trait;
use calm_types::worker::{Liveness, WorkerSession, WorkerSessionId, WorkerSessionState};
use sqlx::{Sqlite, Transaction};

use crate::error::Result;
use crate::ids::TrackId;

pub type Tx<'a> = Transaction<'a, Sqlite>;

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum CommitExitOutcome {
    Committed(WorkerSession),
    Absorbed,
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

    /// The subset of `thread_ids` whose codex session POSITIVELY ended: an `exited`/`failed` row
    /// names the thread and no other row does. A `superseded` row is not an end (a failed start
    /// restores it), and a thread no row names yet is never returned.
    async fn codex_threads_ended(&self, thread_ids: &[String]) -> Result<Vec<String>>;
}
