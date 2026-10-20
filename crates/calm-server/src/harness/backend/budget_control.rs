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

pub(crate) enum BudgetControlScope {
    Historical,
    Canonical(Box<ExecutionEndTarget>),
}

pub(crate) struct ControlAttemptReport {
    delivery: ControlDelivery,
    outcome: Result<TurnId>,
}

impl ControlAttemptReport {
    pub(crate) fn delivery(&self) -> &ControlDelivery {
        &self.delivery
    }
    pub(crate) fn outcome(&self) -> &Result<TurnId> {
        &self.outcome
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
        scope: BudgetControlScope,
        admission: &crate::harness::native_admission::NativePromptAdmission<'_>,
    ) -> ControlAttemptReport {
        let Arm::Codex(daemon, owner) = &self.0 else {
            return ControlAttemptReport {
                delivery: ControlDelivery::NotDispatched,
                outcome: Err(CalmError::Conflict(
                    "provider does not support execution-budget control".into(),
                )),
            };
        };
        if let BudgetControlScope::Canonical(target) = &scope
            && (!owner.binding.matches(target)
                || !matches!(&target.scope,
                ExecutionEndScope::NativeTurn {thread_id, native_turn_id}
                    if thread_id == notice.thread_id() && native_turn_id == notice.turn_id()))
        {
            return ControlAttemptReport {
                delivery: ControlDelivery::NotDispatched,
                outcome: Err(CalmError::Conflict(
                    "budget notice names another execution owner".into(),
                )),
            };
        }
        let native = daemon
            .turn_steer_report(
                notice.thread_id(),
                notice.turn_id(),
                vec![InputItem::Text {
                    text: notice.text(),
                }],
                Some(notice.client_id()),
                match scope {
                    BudgetControlScope::Historical => {
                        crate::shared_codex_appserver::NativeSteer::BudgetControl
                    }
                    BudgetControlScope::Canonical(target) => {
                        crate::shared_codex_appserver::NativeSteer::CanonicalControl(target)
                    }
                },
                admission,
            )
            .await;
        let delivery = match native.delivery() {
            CodexPromptDelivery::NotDispatched => ControlDelivery::NotDispatched,
            CodexPromptDelivery::Unknown => ControlDelivery::Unknown,
            CodexPromptDelivery::Accepted { turn_id } => ControlDelivery::Accepted {
                native_turn_id: turn_id.clone(),
            },
        };
        ControlAttemptReport {
            delivery,
            outcome: native.into_outcome(),
        }
    }

    pub(crate) async fn interrupt_execution(
        &self,
        target: &ExecutionEndTarget,
        admission: &crate::harness::native_admission::NativePromptAdmission<'_>,
    ) -> Result<()> {
        match &self.0 {
            Arm::Codex(daemon, owner) if owner.binding.matches(target) => {
                daemon.interrupt_execution(target, admission).await
            }
            Arm::Claude(session) => session.interrupt_execution(target, admission).await,
            Arm::Codex(..) | Arm::Acp(_) => Err(CalmError::Conflict(
                "control requires this backend's original native owner".into(),
            )),
        }
    }
}
