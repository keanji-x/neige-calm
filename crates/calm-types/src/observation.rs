//! Spec-harness observation vocabulary (#679 PR1).
//!
//! [`Observation`] is the unit the kernel pushes into an agent session
//! (today: the spec harness queue; tomorrow: any planner session via
//! calm-exec's `ObservationSink`). It is pure data — persisted inside
//! `HarnessSnapshot.pending_queue` (Tier-A `handle_state_json` contract)
//! and replayed on boot — so it lives in the vocabulary crate. The queue,
//! debounce and turn-issuance machinery around it stay in calm-server's
//! `harness` module.

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
    WorkspaceLeased {
        wave_id: WaveId,
        card_id: CardId,
        lease_id: String,
        path: String,
    },
    WorkspaceReleased {
        wave_id: WaveId,
        card_id: CardId,
        lease_id: String,
    },
    ForgePrMerged {
        wave_id: WaveId,
        pr_number: u64,
    },
    ForgeScanCompleted {
        wave_id: WaveId,
        overlapping_prs: Vec<u64>,
    },
    ForgePrOpened {
        wave_id: WaveId,
        pr_number: u64,
    },
    ForgePrChecks {
        wave_id: WaveId,
        pr_number: u64,
        conclusion: String,
    },
    ForgeIssueClosed {
        wave_id: WaveId,
        issue_number: u64,
    },
    WorktreeProvisioned {
        wave_id: WaveId,
        card_id: CardId,
        path: String,
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
                | Observation::ForgePrMerged { .. }
                | Observation::ForgeScanCompleted { .. }
                | Observation::ForgePrOpened { .. }
                | Observation::ForgePrChecks { .. }
                | Observation::ForgeIssueClosed { .. }
                | Observation::WorktreeProvisioned { .. }
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
            Observation::WorkspaceLeased { path, .. } => {
                format!("A worker workspace was provisioned at {path}. Re-read the wave state.")
            }
            Observation::WorkspaceReleased { .. } => {
                "A worker workspace lease was released. Re-read the wave state.".to_string()
            }
            Observation::ForgePrMerged { pr_number, .. } => {
                format!("Forge PR #{pr_number} was merged. Re-read the wave state.")
            }
            Observation::ForgeScanCompleted {
                overlapping_prs, ..
            } => format!(
                "Forge scan completed with overlapping PRs {:?}. Re-read the wave state.",
                overlapping_prs
            ),
            Observation::ForgePrOpened { pr_number, .. } => {
                format!("Forge PR #{pr_number} was opened. Re-read the wave state.")
            }
            Observation::ForgePrChecks {
                pr_number,
                conclusion,
                ..
            } => format!(
                "Forge checks completed for PR #{pr_number} with conclusion {conclusion}. Re-read the wave state."
            ),
            Observation::ForgeIssueClosed { issue_number, .. } => {
                format!("Forge issue #{issue_number} was closed. Re-read the wave state.")
            }
            Observation::WorktreeProvisioned { path, .. } => {
                format!("A worker git worktree was provisioned at {path}. Re-read the wave state.")
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
