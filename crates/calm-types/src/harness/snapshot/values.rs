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
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// A rewind the provider applies when the conversation's next turn starts (#1923).
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
    /// Positive V2 rewind receipts are bound to their native thread.
    CodexV2 {
        thread_id: String,
        before_turn_id: String,
        reverted: bool,
    },
    /// `--resume-session-at` / `--resume-drops-turn` on the next spawn.
    Claude { resume_at: Uuid, drops_turn: Uuid },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};

    fn assert_wire<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(
        value: T,
        wire: Value,
    ) {
        assert_eq!(serde_json::to_value(&value).unwrap(), wire);
        assert_eq!(serde_json::from_value::<T>(wire).unwrap(), value);
    }

    #[test]
    fn snapshot_values_keep_their_v1_wire_shapes() {
        assert_wire(
            TokenUsage {
                used_tokens: 10,
                total_tokens: 20,
                context_window: None,
                at_ms: 3,
            },
            json!({"used_tokens": 10, "total_tokens": 20, "context_window": null, "at_ms": 3}),
        );
        assert_wire(
            CompactOutcome {
                operation_id: "op".into(),
                thread_id: "thread".into(),
                turn_id: None,
                status: CompactStatus::Unconfirmed,
            },
            json!({"operation_id": "op", "thread_id": "thread", "turn_id": null, "status": "unconfirmed"}),
        );
        assert_wire(QueueEntryId::from_wire("entry".into()), json!("entry"));
        assert_wire(
            TurnBase {
                turn_id: "turn".into(),
                seen_head: Some("head".into()),
            },
            json!({"turn_id": "turn", "seen_head": "head"}),
        );
    }

    #[test]
    fn persisted_rewind_keeps_its_v1_provider_tags() {
        assert_wire(
            PersistedRewind::Codex {
                before_turn_id: "turn".into(),
                reverted: false,
            },
            json!({"provider": "codex", "before_turn_id": "turn"}),
        );
        assert_wire(
            PersistedRewind::Codex {
                before_turn_id: "turn".into(),
                reverted: true,
            },
            json!({"provider": "codex", "before_turn_id": "turn", "reverted": true}),
        );
        let resume_at = Uuid::from_u128(1);
        let drops_turn = Uuid::from_u128(2);
        assert_wire(
            PersistedRewind::Claude {
                resume_at,
                drops_turn,
            },
            json!({"provider": "claude", "resume_at": resume_at, "drops_turn": drops_turn}),
        );
    }
}
