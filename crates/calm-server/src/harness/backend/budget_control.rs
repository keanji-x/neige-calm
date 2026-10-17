//! A closed execution-budget command, not a work-input or submission ingress.
use super::*;
use crate::planner_turn_budget::BudgetControlNotice;
use crate::shared_codex_appserver::CodexPromptDelivery;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ControlDelivery {
    NotDispatched,
    Unknown,
    Accepted { native_turn_id: String },
}

pub(crate) struct ControlAttemptReport {
    notice: BudgetControlNotice,
    delivery: ControlDelivery,
    rpc_failure: Option<NativeRpcFailure>,
    outcome: Result<TurnId>,
}

impl ControlAttemptReport {
    pub(crate) fn notice(&self) -> &BudgetControlNotice {
        &self.notice
    }
    pub(crate) fn delivery(&self) -> &ControlDelivery {
        &self.delivery
    }
    pub(crate) fn rpc_failure(&self) -> Option<&NativeRpcFailure> {
        self.rpc_failure.as_ref()
    }
    pub(crate) fn outcome(&self) -> &Result<TurnId> {
        &self.outcome
    }
    pub(crate) fn into_outcome(self) -> Result<TurnId> {
        self.outcome
    }
}

impl PlannerBackend {
    /// Independent from permission to append human work in a shared conversation.
    pub(crate) fn supports_budget_control(&self) -> bool {
        matches!(&self.0, Arm::Codex(..))
    }

    pub(crate) async fn send_budget_notice(
        &self,
        notice: &BudgetControlNotice,
    ) -> ControlAttemptReport {
        let Arm::Codex(daemon, _) = &self.0 else {
            return ControlAttemptReport {
                notice: notice.clone(),
                delivery: ControlDelivery::NotDispatched,
                rpc_failure: None,
                outcome: Err(CalmError::Conflict(
                    "provider does not support execution-budget control".into(),
                )),
            };
        };
        let native = daemon
            .turn_steer_report(
                notice.thread_id(),
                notice.turn_id(),
                vec![InputItem::Text {
                    text: notice.text(),
                }],
                Some(notice.client_id()),
                crate::shared_codex_appserver::NativeSteer::BudgetControl,
                &|| Box::pin(async { Ok(()) }),
            )
            .await;
        let delivery = match native.delivery() {
            CodexPromptDelivery::NotDispatched => ControlDelivery::NotDispatched,
            CodexPromptDelivery::Unknown => ControlDelivery::Unknown,
            CodexPromptDelivery::Accepted { turn_id } => ControlDelivery::Accepted {
                native_turn_id: turn_id.clone(),
            },
        };
        let rpc_failure = native.rpc_failure_ref().map(NativeRpcFailure::from_rpc);
        ControlAttemptReport {
            notice: notice.clone(),
            delivery,
            rpc_failure,
            outcome: native.into_outcome(),
        }
    }
}
