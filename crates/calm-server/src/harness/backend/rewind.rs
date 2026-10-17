//! Pure anchor planning is separate from the checkpointed native effect.
use super::*;

#[derive(Debug, thiserror::Error)]
pub(crate) enum RewindPlanFailure {
    #[error("{0}")]
    Unsupported(&'static str),
    #[error(transparent)]
    Invalid(#[from] CalmError),
}

impl RewindPlanFailure {
    pub(crate) fn into_error(self) -> CalmError {
        match self {
            Self::Unsupported(reason) => CalmError::Conflict(reason.into()),
            Self::Invalid(error) => error,
        }
    }
}

impl PlannerBackend {
    /// Ordinary pending input edits need no native history replacement.
    pub(crate) fn history_replacement_supported(&self) -> bool {
        matches!(&self.0, Arm::Codex(..))
    }

    pub(crate) fn plan_rewind(
        &self,
        thread_id: &str,
        target: RewindTarget<'_>,
    ) -> std::result::Result<BackendRewind, RewindPlanFailure> {
        match &self.0 {
            Arm::Codex(..) => {
                if target.turn_id.is_empty() || thread_id.is_empty() {
                    return Err(CalmError::Conflict(
                        "history replacement requires a native turn".into(),
                    )
                    .into());
                }
                Ok(BackendRewind(RewindArm::CodexV2 {
                    thread_id: thread_id.into(),
                    before_turn_id: target.turn_id.into(),
                    reverted: false,
                }))
            }
            Arm::Claude(_) => {
                // Validate the existing native anchors without launching the CLI. Its exit status
                // alone cannot establish that a cut applied, or authorize repeating cut flags.
                crate::claude_planner::rewind::truncation(
                    target.previous_turn,
                    target.prompt_client_id,
                )?;
                Err(RewindPlanFailure::Unsupported(
                    "Claude native history replacement is unavailable without a confirmed cut receipt",
                ))
            }
            Arm::Acp(_) => Err(RewindPlanFailure::Unsupported(
                "ACP native history replacement is unsupported",
            )),
        }
    }

    /// Call only after the owning snapshot records Unknown and these native anchors.
    /// An unknown prior effect must never enter this function again.
    pub(crate) async fn apply_rewind(
        &self,
        thread_id: &str,
        plan: &mut BackendRewind,
    ) -> RewindReport {
        match (&self.0, &mut plan.0) {
            (Arm::Codex(_, _), RewindArm::CodexV2 { thread_id: bound, reverted: true, .. }) if bound == thread_id => RewindReport {
                thread_id: thread_id.into(),
                observation: RewindObservation::Applied(plan.clone()), outcome: Ok(()),
            },
            (Arm::Codex(daemon, _), RewindArm::CodexV2 { thread_id: bound, before_turn_id, reverted }) if bound == thread_id => {
                let outcome = revert_codex_thread(daemon, thread_id, before_turn_id).await;
                let observation = if outcome.is_ok() {
                    *reverted = true;
                    RewindObservation::Applied(plan.clone())
                } else {
                    RewindObservation::Unknown(plan.clone())
                };
                RewindReport { thread_id: thread_id.into(), observation, outcome }
            }
            _ => RewindReport {
                thread_id: thread_id.into(),
                observation: RewindObservation::Unknown(plan.clone()),
                outcome: Err(TurnStartFailure::Refused {
                    error: CalmError::Conflict("native history replacement is unsupported for this backend".into()),
                    reader: "This provider cannot safely replace native history. This call attempted no cut; prior effects remain unresolved.".into(),
                }),
            },
        }
    }
}
