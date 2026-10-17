//! Runtime metadata shared by the frozen V1 wire adapter and body-free V2 frame.
//! Queue/body representation belongs to each version, never this constructor.
use super::*;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HarnessRuntimeHeader {
    pub phase: HarnessPhaseTag,
    pub turn_budget: FrozenBudget,
    pub push_watermark: i64,
    pub last_thread_id: Option<String>,
    pub last_turn_id: Option<String>,
    pub last_report_body_sha256: Option<String>,
    pub last_seen_head: Option<String>,
    pub issued_turn_head: Option<String>,
    pub wedged_reason: Option<String>,
    pub interruption_intent: Option<calm_types::harness::HarnessInterruptionIntent>,
    pub token_usage: Option<TokenUsage>,
    pub compact_outcome: Option<super::super::compact_outcome::CompactOutcome>,
    pub context_usage_invalidated: bool,
    pub context_usage_invalidated_turn: Option<String>,
    pub pending_rewind: Option<BackendRewind>,
    pub last_turn_base: Option<TurnBase>,
}

impl HarnessRuntimeHeader {
    pub(super) fn initial(push_watermark: i64) -> Self {
        Self::from_state(
            &HarnessState::PendingThreadStart,
            FrozenBudget::Legacy,
            push_watermark,
            None,
            None,
            None,
        )
    }

    pub(crate) fn from_state(
        state: &HarnessState,
        turn_budget: FrozenBudget,
        push_watermark: i64,
        last_thread_id: Option<String>,
        last_turn_id: Option<String>,
        last_report_body_sha256: Option<String>,
    ) -> Self {
        Self {
            phase: HarnessPhaseTag::from(state),
            turn_budget,
            push_watermark,
            last_thread_id,
            last_turn_id,
            last_report_body_sha256,
            last_seen_head: None,
            issued_turn_head: None,
            wedged_reason: match state {
                HarnessState::Wedged { reason, .. } => Some(reason.clone()),
                _ => None,
            },
            interruption_intent: match state {
                HarnessState::Issuing {
                    kind:
                        IssuingKind::Interrupt {
                            target_turn_id,
                            reason,
                        },
                    ..
                } => calm_types::harness::HarnessInterruptionIntent::from_request(
                    target_turn_id,
                    reason,
                ),
                _ => None,
            },
            token_usage: None,
            compact_outcome: None,
            context_usage_invalidated: false,
            context_usage_invalidated_turn: None,
            pending_rewind: None,
            last_turn_base: None,
        }
    }
}
