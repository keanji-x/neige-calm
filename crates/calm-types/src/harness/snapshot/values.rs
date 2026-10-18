//! IO-free values in the persisted harness header; effects and policy stay in their owners.
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Latest context-window usage observed on a harness thread; latest-wins, so it rides the
/// runtime snapshot rather than `harness_items`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// `tokenUsage.last.totalTokens` — the occupancy proxy; this, never `total_tokens`, is what a
    /// percentage may divide.
    pub used_tokens: i64,
    /// `tokenUsage.total.totalTokens` — the thread's lifetime sum; unbounded, meaningless as a fill ratio.
    pub total_tokens: i64,
    /// `tokenUsage.modelContextWindow`. `None` means "codex has not told us", and a `None` after a
    /// known window does not erase it; the server merge owner preserves it.
    pub context_window: Option<i64>,
    /// Wall clock of the frame that produced this value, shipped on `GET /planner/run` so the UI
    /// can tell a live number from one rehydrated out of an old snapshot.
    pub at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompactStatus {
    Started,
    Completed,
    Interrupted,
    Failed,
    Unconfirmed,
    Refused,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CompactOutcome {
    /// Neige's identifier for this submission, minted before calling the provider.
    pub operation_id: String,
    pub thread_id: String,
    /// Unknown until the provider starts its maintenance turn.
    #[schema(required = true)]
    pub turn_id: Option<String>,
    pub status: CompactStatus,
}

/// Stable identity for one addressable user entry in the pending queue.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct QueueEntryId(String);

impl QueueEntryId {
    /// Adopt an id that arrived from a client, verbatim; an id from a URL is a lookup key, never a claim.
    pub fn from_wire(id: String) -> Self {
        Self(id)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for QueueEntryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Native turn and original version-control watermark stored by the harness.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TurnBase {
    pub turn_id: String,
    pub seen_head: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum PersistedRewind {
    /// `thread/revert {threadId, beforeTurnId}` before the next `turn/start`, unless `reverted`
    /// records that it already went through (#2512). Additive and defaulted, written only when
    /// set, so a rolled-back binary still reads the snapshot: it ignores the key and reverts again.
    Codex {
        before_turn_id: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        reverted: bool,
    },
    /// C3 binds every positive rewind receipt to its native thread. The V1 arm above
    /// remains byte-compatible, but cannot authorize a V2 dispatch.
    CodexV2 {
        thread_id: String,
        before_turn_id: String,
        reverted: bool,
    },
    /// `--resume-session-at` / `--resume-drops-turn` on the next spawn.
    Claude { resume_at: Uuid, drops_turn: Uuid },
}
