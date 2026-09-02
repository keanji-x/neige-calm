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

use crate::event::{EditAuthor, RatifyDecision};
use crate::ids::{CardId, WaveId};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Observation {
    WaveGoal {
        text: String,
    },
    /// A `wave.report_edited` the dispatcher decided warrants waking the
    /// spec (`dispatcher::SPEC_WAKE_AUTHORS` — `user` / `plugin` /
    /// `assistant`, never the spec's own writes).
    ReportEdited {
        wave_id: WaveId,
        body_sha256: String,
        body: String,
        /// #1252 S0 R1/F2 — who made the edit. The spec system prompt tells
        /// the agent to consider the edit's `author`, so the turn text has
        /// to carry one; before this it hardcoded "The user edited …" and
        /// mislabelled every plugin/assistant edit as a user edit.
        ///
        /// The `Option` is load-bearing (`#[serde(default)]` below is
        /// explicit reinforcement of serde's own "absent `Option` is `None`"
        /// rule, not the mechanism): `Observation` is persisted verbatim
        /// inside `HarnessSnapshot.pending_queue`
        /// (`session_projection.handle_state_json`) and read back on boot
        /// recovery, so a required field would fail to deserialize every
        /// already-queued observation and wedge the snapshot. `None` means
        /// "queued before #1252, author unknown" and renders the
        /// byte-identical old sentence so replayed history does not change.
        /// The dispatcher always populates it.
        #[serde(default)]
        author: Option<EditAuthor>,
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
    WorktreeCommitted {
        wave_id: WaveId,
        card_id: CardId,
        commit_sha: String,
        branch: String,
    },
    ReviewRound {
        wave_id: WaveId,
        phase: String,
        slice_id: String,
        pr_number: Option<u64>,
        head_sha: Option<String>,
        n: u32,
        cap: u32,
        converged: bool,
    },
    RatifyRequested {
        wave_id: WaveId,
        reason: String,
    },
    RatifyResolved {
        wave_id: WaveId,
        decision: RatifyDecision,
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
        match self {
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
            | Observation::WorktreeCommitted { .. }
            | Observation::ReviewRound { .. }
            | Observation::RatifyRequested { .. }
            | Observation::RatifyResolved { .. } => true,
            Observation::WaveGoal { .. }
            | Observation::ReportEdited { .. }
            | Observation::WorkspaceLeased { .. }
            | Observation::WorkspaceReleased { .. } => false,
        }
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
            // #1252 S0 R1/F2. `None` is only reachable for observations
            // queued before the `author` field existed; it must render the
            // byte-identical pre-#1252 sentence so replayed history does
            // not change under a reader. Everything the dispatcher enqueues
            // today names its author in the same `author = "..."` spelling
            // the spec system prompt uses.
            Observation::ReportEdited { author: None, .. } => {
                "The user edited the wave report. Re-read the wave state.".to_string()
            }
            Observation::ReportEdited {
                author: Some(author),
                ..
            } => format!(
                "The wave report was edited (author = \"{}\"). Re-read the wave state.",
                author.wire_str()
            ),
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
            Observation::WorktreeCommitted { branch, .. } => {
                format!("A worker git worktree committed branch {branch}. Re-read the wave state.")
            }
            Observation::ReviewRound {
                phase,
                slice_id,
                pr_number,
                head_sha,
                n,
                cap,
                converged,
                ..
            } => {
                let subject = match pr_number {
                    Some(pr) => format!("{phase}/{slice_id}/PR #{pr}"),
                    None => format!("{phase}/{slice_id}/design"),
                };
                let head = head_sha
                    .as_deref()
                    .map(|sha| format!(" at {sha}"))
                    .unwrap_or_default();
                format!(
                    "Review round {n}/{cap} for {subject}{head} recorded converged={converged}. Re-read the wave state."
                )
            }
            Observation::RatifyRequested { reason, .. } => {
                format!("Ratification was requested: {reason}. Re-read the wave state.")
            }
            Observation::RatifyResolved { decision, .. } => {
                let decision = match decision {
                    RatifyDecision::Grant => "grant",
                    RatifyDecision::Deny => "deny",
                };
                format!(
                    "Ratification was resolved with decision={decision}. Re-read the wave state."
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

    fn report_edited(author: Option<EditAuthor>) -> Observation {
        Observation::ReportEdited {
            wave_id: WaveId::from("wave-1"),
            body_sha256: "sha".into(),
            body: "body".into(),
            author,
        }
    }

    /// #1252 S0 R1/F2 — the spec system prompt tells the agent the waking
    /// `wave.report_edited` carries an `author` of `user` / `plugin` /
    /// `assistant`. The turn text used to hardcode "The user edited …", so a
    /// plugin- or assistant-authored edit woke the spec with a sentence that
    /// contradicted the event and the prompt both.
    #[test]
    fn report_edited_turn_text_names_the_real_author() {
        for author in [EditAuthor::Plugin, EditAuthor::Assistant] {
            let text = report_edited(Some(author)).to_turn_text();
            assert!(
                !text.contains("The user edited"),
                "{author:?} edit must not be reported as a user edit: {text}"
            );
            assert!(
                text.contains(&format!("author = \"{}\"", author.wire_str())),
                "{author:?} edit must name its author: {text}"
            );
        }
        let user = report_edited(Some(EditAuthor::User)).to_turn_text();
        assert!(
            user.contains("author = \"user\""),
            "user edit must name its author too: {user}"
        );
    }

    /// #1252 S0 R1/F2 — `Observation` is persisted inside
    /// `HarnessSnapshot.pending_queue` and read back on boot recovery, so a
    /// `ReportEdited` queued before `author` existed must still deserialize,
    /// and must render the byte-identical pre-#1252 sentence: replayed
    /// history may not change under a reader.
    #[test]
    fn legacy_report_edited_without_author_deserializes_and_keeps_old_text() {
        let legacy = serde_json::json!({
            "type": "report_edited",
            "wave_id": "wave-1",
            "body_sha256": "sha",
            "body": "body",
        });
        let obs: Observation =
            serde_json::from_value(legacy).expect("pre-#1252 queued observation must deserialize");
        assert_eq!(obs, report_edited(None));
        assert_eq!(
            obs.to_turn_text(),
            "The user edited the wave report. Re-read the wave state."
        );
    }

    #[test]
    fn review_and_ratify_observations_are_hard_fire() {
        let review = Observation::ReviewRound {
            wave_id: WaveId::from("wave-1"),
            phase: "impl".into(),
            slice_id: "5b".into(),
            pr_number: Some(760),
            head_sha: Some("head-sha".into()),
            n: 2,
            cap: 8,
            converged: true,
        };
        assert!(review.is_hard_fire());
        let text = review.to_turn_text();
        assert!(text.contains("2/8"), "round count missing: {text}");
        assert!(
            text.contains("converged=true"),
            "convergence missing: {text}"
        );

        let requested = Observation::RatifyRequested {
            wave_id: WaveId::from("wave-1"),
            reason: "cap_exhausted".into(),
        };
        assert!(requested.is_hard_fire());
        assert!(requested.to_turn_text().contains("cap_exhausted"));

        let resolved = Observation::RatifyResolved {
            wave_id: WaveId::from("wave-1"),
            decision: RatifyDecision::Grant,
        };
        assert!(resolved.is_hard_fire());
        assert!(resolved.to_turn_text().contains("decision=grant"));
    }
}
