//! Planner-harness observation vocabulary (#679 PR1).
//!
//! [`Observation`] is the unit the kernel pushes into an agent session
//! (today: the planner harness queue; tomorrow: any planner session via
//! calm-exec's `ObservationSink`). It is pure data — persisted inside
//! `HarnessSnapshot.pending_queue` (Tier-A `handle_state_json` contract)
//! and replayed on boot — so it lives in the vocabulary crate. The queue,
//! debounce and turn-issuance machinery around it stay in calm-server's
//! `harness` module.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::event::{EditAuthor, RatifyDecision};
use crate::ids::{CardId, TrackId};
use crate::model::{HarnessInputPresentation, HarnessInputSegment};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Observation {
    TrackGoal {
        text: String,
    },
    /// A `track.report_edited` the dispatcher decided warrants waking the
    /// planner (`dispatcher::PLANNER_WAKE_AUTHORS` — `user` / `plugin` /
    /// `assistant`, never the planner's own writes).
    ReportEdited {
        track_id: TrackId,
        body_sha256: String,
        body: String,
        /// #1252 S0 R1/F2 — who made the edit. The planner system prompt tells
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
        track_id: TrackId,
        card_id: CardId,
        kind: HookKind,
        #[serde(default)]
        idempotency_key: String,
    },
    /// Context supplied by the kernel for an assistant turn, not words the
    /// user typed. It is a distinct variant so the persisted input-segment
    /// presentation cannot render it back to the user as their message.
    SystemContext {
        text: String,
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
    /// REPLACES the suppressed worker self-report as the planner's wake-up
    /// (the planner hears the gate, not the claim). `idempotency_key` is
    /// the task id (`"{track_id}:{key}"`); `key` is the plan key used in
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
        track_id: TrackId,
        card_id: CardId,
        lease_id: String,
        path: String,
    },
    WorkspaceReleased {
        track_id: TrackId,
        card_id: CardId,
        lease_id: String,
    },
    ForgePrMerged {
        track_id: TrackId,
        pr_number: u64,
    },
    ForgeScanCompleted {
        track_id: TrackId,
        overlapping_prs: Vec<u64>,
    },
    ForgePrOpened {
        track_id: TrackId,
        pr_number: u64,
    },
    ForgePrChecks {
        track_id: TrackId,
        pr_number: u64,
        conclusion: String,
    },
    ForgeIssueClosed {
        track_id: TrackId,
        issue_number: u64,
    },
    WorktreeProvisioned {
        track_id: TrackId,
        card_id: CardId,
        path: String,
    },
    WorktreeCommitted {
        track_id: TrackId,
        card_id: CardId,
        commit_sha: String,
        branch: String,
    },
    ReviewRound {
        track_id: TrackId,
        phase: String,
        slice_id: String,
        pr_number: Option<u64>,
        head_sha: Option<String>,
        n: u32,
        cap: u32,
        converged: bool,
    },
    RatifyRequested {
        track_id: TrackId,
        reason: String,
    },
    RatifyResolved {
        track_id: TrackId,
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
    /// Preserve an issued batch as independently attributable segments before
    /// Codex flattens it into one `userMessage`. The same rendered strings feed
    /// `turn/start`, so the persisted structure and model input cannot drift.
    pub fn input_segments_for(observations: &[Self]) -> Vec<HarnessInputSegment> {
        observations
            .iter()
            .map(|observation| HarnessInputSegment {
                presentation: observation.input_presentation(),
                text: observation.to_turn_text(),
            })
            .collect()
    }

    fn input_presentation(&self) -> HarnessInputPresentation {
        match self {
            Observation::TrackGoal { .. } | Observation::UserMessage { .. } => {
                HarnessInputPresentation::User
            }
            Observation::ReportEdited { .. } => HarnessInputPresentation::SystemReportEdited,
            Observation::TaskCompleted { .. } => HarnessInputPresentation::SystemTaskCompleted,
            Observation::TaskFailed { .. } => HarnessInputPresentation::SystemTaskFailed,
            Observation::WorkerHookStop { .. } => {
                HarnessInputPresentation::SystemWorkerTurnFinished
            }
            Observation::SystemContext { .. }
            | Observation::TaskGateResult { .. }
            | Observation::WorkspaceLeased { .. }
            | Observation::WorkspaceReleased { .. }
            | Observation::ForgePrMerged { .. }
            | Observation::ForgeScanCompleted { .. }
            | Observation::ForgePrOpened { .. }
            | Observation::ForgePrChecks { .. }
            | Observation::ForgeIssueClosed { .. }
            | Observation::WorktreeProvisioned { .. }
            | Observation::WorktreeCommitted { .. }
            | Observation::ReviewRound { .. }
            | Observation::RatifyRequested { .. }
            | Observation::RatifyResolved { .. } => HarnessInputPresentation::System,
        }
    }

    pub fn is_hard_fire(&self) -> bool {
        match self {
            Observation::TaskCompleted { .. }
            | Observation::TaskFailed { .. }
            | Observation::WorkerHookStop { .. }
            | Observation::SystemContext { .. }
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
            Observation::TrackGoal { .. }
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
            Observation::TrackGoal { text } => text.clone(),
            Observation::SystemContext { text } => text.clone(),
            Observation::UserMessage { text } => format!("User says:\n{text}"),
            // #1252 S0 R1/F2. `None` is only reachable for observations
            // queued before the `author` field existed; it must render the
            // byte-identical pre-#1252 sentence so replayed history does
            // not change under a reader. Everything the dispatcher enqueues
            // today names its author in the same `author = "..."` spelling
            // the planner system prompt uses.
            Observation::ReportEdited { author: None, .. } => {
                "The user edited the track report. Re-read the track state.".to_string()
            }
            Observation::ReportEdited {
                author: Some(author),
                ..
            } => format!(
                "The track report was edited (author = \"{}\"). Re-read the track state.",
                author.wire_str()
            ),
            Observation::TaskCompleted {
                idempotency_key, ..
            } => format!(
                "A dispatched task completed (idempotency_key={idempotency_key}). Re-read the track state to incorporate its result."
            ),
            Observation::TaskFailed {
                idempotency_key,
                error,
            } => format!(
                "A dispatched task failed (idempotency_key={idempotency_key}): {error}. Re-read the track state and decide how to proceed."
            ),
            Observation::WorkerHookStop {
                idempotency_key, ..
            } => format!(
                "A worker card finished a turn. Re-read the track state to incorporate any changes.\n(hook_id={idempotency_key})"
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
                format!("A worker workspace was provisioned at {path}. Re-read the track state.")
            }
            Observation::WorkspaceReleased { .. } => {
                "A worker workspace lease was released. Re-read the track state.".to_string()
            }
            Observation::ForgePrMerged { pr_number, .. } => {
                format!("Forge PR #{pr_number} was merged. Re-read the track state.")
            }
            Observation::ForgeScanCompleted {
                overlapping_prs, ..
            } => format!(
                "Forge scan completed with overlapping PRs {:?}. Re-read the track state.",
                overlapping_prs
            ),
            Observation::ForgePrOpened { pr_number, .. } => {
                format!("Forge PR #{pr_number} was opened. Re-read the track state.")
            }
            Observation::ForgePrChecks {
                pr_number,
                conclusion,
                ..
            } => format!(
                "Forge checks completed for PR #{pr_number} with conclusion {conclusion}. Re-read the track state."
            ),
            Observation::ForgeIssueClosed { issue_number, .. } => {
                format!("Forge issue #{issue_number} was closed. Re-read the track state.")
            }
            Observation::WorktreeProvisioned { path, .. } => {
                format!("A worker git worktree was provisioned at {path}. Re-read the track state.")
            }
            Observation::WorktreeCommitted { branch, .. } => {
                format!("A worker git worktree committed branch {branch}. Re-read the track state.")
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
                    "Review round {n}/{cap} for {subject}{head} recorded converged={converged}. Re-read the track state."
                )
            }
            Observation::RatifyRequested { reason, .. } => {
                format!("Ratification was requested: {reason}. Re-read the track state.")
            }
            Observation::RatifyResolved { decision, .. } => {
                let decision = match decision {
                    RatifyDecision::Grant => "grant",
                    RatifyDecision::Deny => "deny",
                };
                format!(
                    "Ratification was resolved with decision={decision}. Re-read the track state."
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

    #[test]
    fn input_segments_are_derived_from_observation_types_not_english() {
        let human_with_system_words = Observation::UserMessage {
            text: "A dispatched task completed, according to me".into(),
        };
        assert_eq!(
            Observation::input_segments_for(&[human_with_system_words])[0].presentation,
            HarnessInputPresentation::User,
            "human text must not be classified by its English prefix"
        );

        assert_eq!(
            Observation::input_segments_for(&[Observation::TaskCompleted {
                idempotency_key: "task-1".into(),
                result: serde_json::json!({"ok": true}),
            }])[0]
                .presentation,
            HarnessInputPresentation::SystemTaskCompleted
        );
        assert_eq!(
            Observation::input_segments_for(&[Observation::TaskFailed {
                idempotency_key: "task-1".into(),
                error: "boom".into(),
            }])[0]
                .presentation,
            HarnessInputPresentation::SystemTaskFailed
        );
        assert_eq!(
            Observation::input_segments_for(&[Observation::WorkerHookStop {
                track_id: TrackId::from("track-1"),
                card_id: CardId::from("card-1"),
                kind: HookKind::CodexStop,
                idempotency_key: "hook-1".into(),
            }])[0]
                .presentation,
            HarnessInputPresentation::SystemWorkerTurnFinished
        );
        assert_eq!(
            Observation::input_segments_for(&[report_edited(Some(EditAuthor::Plugin))])[0]
                .presentation,
            HarnessInputPresentation::SystemReportEdited
        );

        let generic = Observation::RatifyRequested {
            track_id: TrackId::from("track-1"),
            reason: "review cap".into(),
        };
        assert_eq!(
            Observation::input_segments_for(&[generic])[0].presentation,
            HarnessInputPresentation::System
        );

        let context = Observation::SystemContext {
            text: "Today is empty".into(),
        };
        assert_eq!(
            Observation::input_segments_for(&[context])[0].presentation,
            HarnessInputPresentation::System,
            "kernel context must never be attributed to the user"
        );
    }

    #[test]
    fn mixed_batch_keeps_each_source_and_rendered_text_in_order() {
        let report = report_edited(Some(EditAuthor::Plugin));
        let human = Observation::UserMessage {
            text: "what happened?".into(),
        };
        let completed = Observation::TaskCompleted {
            idempotency_key: "task-1".into(),
            result: serde_json::Value::Null,
        };
        let expected_text = [
            report.to_turn_text(),
            human.to_turn_text(),
            completed.to_turn_text(),
        ];
        let segments = Observation::input_segments_for(&[report, human, completed]);
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.presentation)
                .collect::<Vec<_>>(),
            vec![
                HarnessInputPresentation::SystemReportEdited,
                HarnessInputPresentation::User,
                HarnessInputPresentation::SystemTaskCompleted,
            ]
        );
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>(),
            expected_text.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert!(Observation::input_segments_for(&[]).is_empty());
    }

    fn report_edited(author: Option<EditAuthor>) -> Observation {
        Observation::ReportEdited {
            track_id: TrackId::from("track-1"),
            body_sha256: "sha".into(),
            body: "body".into(),
            author,
        }
    }

    /// #1252 S0 R1/F2 — the planner system prompt tells the agent the waking
    /// `track.report_edited` carries an `author` of `user` / `plugin` /
    /// `assistant`. The turn text used to hardcode "The user edited …", so a
    /// plugin- or assistant-authored edit woke the planner with a sentence that
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
            "track_id": "track-1",
            "body_sha256": "sha",
            "body": "body",
        });
        let obs: Observation =
            serde_json::from_value(legacy).expect("pre-#1252 queued observation must deserialize");
        assert_eq!(obs, report_edited(None));
        assert_eq!(
            obs.to_turn_text(),
            "The user edited the track report. Re-read the track state."
        );
    }

    #[test]
    fn review_and_ratify_observations_are_hard_fire() {
        let review = Observation::ReviewRound {
            track_id: TrackId::from("track-1"),
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
            track_id: TrackId::from("track-1"),
            reason: "cap_exhausted".into(),
        };
        assert!(requested.is_hard_fire());
        assert!(requested.to_turn_text().contains("cap_exhausted"));

        let resolved = Observation::RatifyResolved {
            track_id: TrackId::from("track-1"),
            decision: RatifyDecision::Grant,
        };
        assert!(resolved.is_hard_fire());
        assert!(resolved.to_turn_text().contains("decision=grant"));
    }
}
