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

    /// Resolve the original local control owner before durable Stop acceptance.
    /// Preparation grants no permission to write; native sends still consume the
    /// canonical exact-target/intent admission at their actual writer boundary.
    pub(crate) async fn prepare_interrupt(
        &self,
        target: &ExecutionEndTarget,
    ) -> Result<PreparedInterrupt> {
        match &self.0 {
            Arm::Codex(daemon, owner) if owner.binding.matches(target) => {
                Ok(PreparedInterrupt::Codex {
                    daemon: daemon.clone(),
                    target: Box::new(target.clone()),
                })
            }
            Arm::Claude(session) => session
                .prepare_interrupt(target)
                .await
                .map(PreparedInterrupt::Claude),
            Arm::Acp(session) => session
                .prepare_interrupt(target)
                .await
                .map(PreparedInterrupt::Acp),
            Arm::Codex(..) => Err(CalmError::Conflict(
                "control requires this backend's original native owner".into(),
            )),
        }
    }

    #[cfg(test)]
    pub(crate) async fn interrupt_execution(
        &self,
        target: &ExecutionEndTarget,
        admission: &crate::harness::native_admission::NativePromptAdmission<'_>,
    ) -> Result<()> {
        self.prepare_interrupt(target).await?.send(admission).await
    }
}

pub(crate) enum PreparedInterrupt {
    Codex {
        daemon: Arc<SharedCodexAppServer>,
        target: Box<ExecutionEndTarget>,
    },
    Claude(crate::claude_planner::session::PreparedInterrupt),
    Acp(crate::acp_planner::session::PreparedInterrupt),
}
impl PreparedInterrupt {
    pub(crate) async fn send(
        self,
        admission: &crate::harness::native_admission::NativePromptAdmission<'_>,
    ) -> Result<()> {
        match self {
            // Connection acquisition remains after acceptance. The native owner
            // is still revalidated there; accepted drain semantics are unchanged.
            Self::Codex { daemon, target } => daemon.interrupt_execution(&target, admission).await,
            Self::Claude(prepared) => prepared.send(admission).await,
            Self::Acp(prepared) => prepared.send(admission).await,
        }
    }
}
