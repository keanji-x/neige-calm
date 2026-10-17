//! Adapt the current server FSM to its declared persisted header.
use super::*;
pub(super) use calm_types::harness::snapshot::HarnessRuntimeHeader;

pub(super) trait HarnessRuntimeHeaderExt {
    fn initial(push_watermark: i64) -> Self;
    fn from_state(
        state: &HarnessState,
        turn_budget: FrozenBudget,
        push_watermark: i64,
        last_thread_id: Option<String>,
        last_turn_id: Option<String>,
        last_report_body_sha256: Option<String>,
    ) -> Self;
}

impl HarnessRuntimeHeaderExt for HarnessRuntimeHeader {
    fn initial(push_watermark: i64) -> Self {
        Self::from_state(
            &HarnessState::PendingThreadStart,
            FrozenBudget::Legacy,
            push_watermark,
            None,
            None,
            None,
        )
    }

    fn from_state(
        state: &HarnessState,
        turn_budget: FrozenBudget,
        push_watermark: i64,
        last_thread_id: Option<String>,
        last_turn_id: Option<String>,
        last_report_body_sha256: Option<String>,
    ) -> Self {
        let mut header = Self::new(
            HarnessPhaseTag::from(state),
            turn_budget,
            push_watermark,
            last_thread_id,
            last_turn_id,
            last_report_body_sha256,
        );
        header.wedged_reason = match state {
            HarnessState::Wedged { reason, .. } => Some(reason.clone()),
            _ => None,
        };
        header.interruption_intent = match state {
            HarnessState::Issuing {
                kind:
                    IssuingKind::Interrupt {
                        target_turn_id,
                        reason,
                    },
                ..
            } => {
                calm_types::harness::HarnessInterruptionIntent::from_request(target_turn_id, reason)
            }
            _ => None,
        };
        header
    }
}
