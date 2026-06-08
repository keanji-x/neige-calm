use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{CardId, WaveId};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Observation {
    WaveGoal {
        text: String,
    },
    ReportEdited {
        wave_id: WaveId,
        body_sha256: String,
        body: String,
    },
    TaskCompleted {
        idempotency_key: String,
        result: Value,
    },
    TaskFailed {
        idempotency_key: String,
        error: String,
    },
    WorkerHookStop {
        wave_id: WaveId,
        card_id: CardId,
        kind: HookKind,
        #[serde(default)]
        idempotency_key: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookKind {
    CodexStop,
    ClaudeStop,
}

impl Observation {
    pub fn is_hard_fire(&self) -> bool {
        matches!(
            self,
            Observation::TaskCompleted { .. }
                | Observation::TaskFailed { .. }
                | Observation::WorkerHookStop { .. }
        )
    }

    pub fn report_sha256(&self) -> Option<&str> {
        match self {
            Observation::ReportEdited { body_sha256, .. } => Some(body_sha256),
            _ => None,
        }
    }

    pub fn to_turn_text(&self) -> String {
        match self {
            Observation::WaveGoal { text } => text.clone(),
            Observation::ReportEdited { .. } => {
                "The user edited the wave report. Re-read the wave state.".to_string()
            }
            Observation::TaskCompleted {
                idempotency_key, ..
            } => format!(
                "A dispatched task completed (idempotency_key={idempotency_key}). Re-read the wave state to incorporate its result."
            ),
            Observation::TaskFailed {
                idempotency_key,
                error,
            } => format!(
                "A dispatched task failed (idempotency_key={idempotency_key}): {error}. Re-read the wave state and decide how to proceed."
            ),
            Observation::WorkerHookStop {
                idempotency_key, ..
            } => format!(
                "A worker card finished a turn. Re-read the wave state to incorporate any changes.\n(hook_id={idempotency_key})"
            ),
        }
    }
}
