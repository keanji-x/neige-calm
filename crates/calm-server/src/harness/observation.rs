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
    /// Review fold-in (#609): forwarded to the LLM as a user message.
    /// Hard-fired so the new turn issues immediately after the current
    /// turn completes (no debounce idle wait) and so the queue cannot
    /// evict it under backpressure. Does NOT interrupt in-flight turns
    /// (`can_issue_turn()` still gates new-turn issuance).
    UserMessage {
        text: String,
    },
    /// Issue #644 PR-C (§6.5) — the kernel gate runner recorded a
    /// verdict for one gate attempt. Hard-fired: for a gated task this
    /// REPLACES the suppressed worker self-report as the spec's wake-up
    /// (the spec hears the gate, not the claim). `idempotency_key` is
    /// the task id (`"{wave_id}:{key}"`); `key` is the plan key used in
    /// the turn-text paths (`plan/<key>/gate.log`, `runs/<task.id>.md`).
    TaskGateResult {
        idempotency_key: String,
        key: String,
        passed: bool,
        #[serde(default)]
        failing_step: Option<String>,
        #[serde(default)]
        exit_code: Option<i32>,
        log_tail: String,
        attempt: i64,
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
                | Observation::UserMessage { .. }
                | Observation::TaskGateResult { .. }
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
            Observation::UserMessage { text } => format!("User says:\n{text}"),
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
            // §6.5 turn text. `failing_step` is absent on
            // timeout/infra verdicts (no step sentinel attributed) —
            // the log tail carries the reason there.
            Observation::TaskGateResult {
                idempotency_key,
                key,
                passed,
                failing_step,
                exit_code,
                log_tail,
                attempt,
            } => {
                let verdict = if *passed {
                    "passed".to_string()
                } else {
                    match (failing_step.as_deref(), exit_code) {
                        (Some(step), Some(code)) => {
                            format!("FAILED at step {step} (exit {code})")
                        }
                        (Some(step), None) => format!("FAILED at step {step}"),
                        (None, Some(code)) => format!("FAILED (exit {code})"),
                        (None, None) => "FAILED".to_string(),
                    }
                };
                format!(
                    "Task {key} gate {verdict} (attempt {attempt}). Log tail:\n{log_tail}\nRead the full log at plan/{key}/gate.log; read the worker output at runs/{idempotency_key}.md."
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_is_hard_fire() {
        let obs = Observation::UserMessage { text: "hi".into() };
        assert!(obs.is_hard_fire());
    }

    #[test]
    fn user_message_to_turn_text_includes_framing() {
        let obs = Observation::UserMessage {
            text: "Did you check Korean refiners?".into(),
        };
        let text = obs.to_turn_text();
        assert!(
            text.starts_with("User says:"),
            "framing prefix missing: {text}"
        );
        assert!(
            text.contains("Did you check Korean refiners?"),
            "raw text missing: {text}"
        );
    }
}
