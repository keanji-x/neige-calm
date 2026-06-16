use async_trait::async_trait;
use calm_types::model::WaveLifecycle;
use calm_types::worker::{Liveness, WorkerSession, WorkerSessionId, WorkerSessionState};
use sqlx::{Sqlite, Transaction};

use crate::error::Result;
use crate::ids::{CoveId, WaveId};

pub type Tx<'a> = Transaction<'a, Sqlite>;

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum CommitExitOutcome {
    Committed(WorkerSession),
    Absorbed,
}

/// #741-4 (DR-4) — a wave the reaper's dead-root scan has identified as having
/// a POSITIVELY-dead root, eligible for `Draft|Planning → Failed` convergence.
///
/// The candidate set is computed entirely in SQL ([`SessionRepo::dead_root_candidates`])
/// so the soundness predicate lives in one auditable place: a candidate is
/// emitted ONLY on a positive dead signal (a failed start-op for a `Draft`
/// wave, or a NULL/terminal root for a `Planning` wave) AND only when NO
/// active planner-contract session exists for the wave (the mid-respawn
/// exclusion). A live or merely just-created wave is never a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadRootCandidate {
    pub wave_id: WaveId,
    pub cove_id: CoveId,
    /// The current lifecycle — always [`WaveLifecycle::Draft`] (failed-start)
    /// or [`WaveLifecycle::Planning`] (lost-root). The reaper drives the
    /// matching `from → Failed` edge and treats a current != from read as a
    /// race-loss.
    pub lifecycle: WaveLifecycle,
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

    /// T2 durable codex worker-liveness feeder (#741 §1.3). Stamps the push-fed
    /// `last_activity_ms` / `last_thread_status` columns on an *active* session
    /// without touching `updated_at_ms`. Benign no-op on a terminal/missing row.
    async fn session_record_activity(
        &self,
        id: &WorkerSessionId,
        last_activity_ms: i64,
        last_thread_status: &str,
    ) -> Result<()>;

    /// T2 durable codex worker-liveness feeder (#741 §1.3), keyed by codex
    /// `thread_id`. The durable notification subscriber only sees thread ids,
    /// so it writes through this. Pinned to `provider='codex'`, never touches
    /// `updated_at_ms`, and is a benign no-op on a terminal/missing row.
    async fn session_record_activity_by_thread(
        &self,
        thread_id: &str,
        last_activity_ms: i64,
        last_thread_status: &str,
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

    async fn session_list_by_wave(&self, wave_id: &WaveId) -> Result<Vec<WorkerSession>>;

    /// #741-4 (DR-4) — scan for waves whose ROOT is POSITIVELY dead, scoped to
    /// the DR-1 terminal edges (`Draft`, `Planning` only). The CARDINAL SAFETY
    /// RULE — never converge a live or merely just-created wave — is enforced
    /// inside the SQL: a `Draft` wave is a candidate only if its
    /// `spec-harness-start` operation resolved to `phase='failed'` (a
    /// pending/succeeded/absent start-op ⇒ NOT dead); a `Planning` wave only if
    /// its root session is NULL or terminal/missing. BOTH arms additionally
    /// require that NO active planner-contract `worker_session`
    /// (state ∈ starting/running/idle/turn_pending) exists for the wave — the
    /// mid-respawn exclusion, which also keeps a still-alive codex root (whose
    /// session is `is_active_authority`) from ever being declared dead on a
    /// bare PTY-`Exited`. Boot-gating is the caller's responsibility (§DR-5).
    async fn dead_root_candidates(&self) -> Result<Vec<DeadRootCandidate>>;
}
