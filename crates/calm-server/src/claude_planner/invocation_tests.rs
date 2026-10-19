#![cfg(test)]
//! Crate-local callers of the production neutral Claude entry, without a public fixture seam.
use crate::error::CalmError;
use crate::harness::backend::{StartDiagnostic, TurnStartFailure};
use crate::planner_model::TurnModelSelection;
use crate::planner_permission_mode::PlannerPermissionMode;
use provider::control::*;

fn ordinary_start(
    outcome: TurnStartOutcome<CalmError, StartDiagnostic>,
) -> Result<String, TurnStartFailure> {
    crate::harness::backend::ordinary_start_result(Ok(outcome))
}

mod claude_planner_session;
mod claude_planner_session_approvals;
#[path = "../../tests/cases/claude_planner_session_fixture.rs"]
mod claude_planner_session_fixture;
#[cfg(test)]
mod claude_planner_session_lifecycle_tests;
mod claude_planner_session_results;
mod claude_planner_stop;

mod claude_planner_retirement;
// This shared stack rig also serves the broader integration suite.
#[allow(dead_code)]
#[path = "../../tests/cases/claude_planner_stack_fixture.rs"]
mod claude_planner_stack_fixture;

fn request(
    items: Vec<provider::InputItem>,
    selection: &TurnModelSelection,
    permission: PlannerPermissionMode,
    client: &str,
) -> TurnRequest {
    TurnRequest {
        input: InputRequest {
            items,
            correlation: CorrelationKey(client.into()),
        },
        selection: selection.clone(),
        permission,
        edit: TurnEdit::NoEdit,
    }
}
