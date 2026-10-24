//! The pure halves of settlement: the §6.2 table and the §5.2 init checks.

use uuid::Uuid;

use super::compact::{NO_BOUNDARY, result_event};
use super::driver::{TerminalEvent, decide};
use super::role::{PlannerRole, init_check};
use super::spawn::SpawnPurpose;
use crate::claude_planner::turn_slot::TerminalCause;
use provider::claude::protocol::{McpStatus, PluginRef, SystemInit};
use provider::claude::translate::TurnOutcome;

fn success(is_error: bool) -> TerminalEvent {
    TerminalEvent::ResultSuccess {
        is_error,
        result: "Not logged in · Please run /login".into(),
    }
}

fn failed(message: &str) -> TurnOutcome {
    TurnOutcome::Failed {
        message: message.into(),
    }
}

#[test]
fn the_terminal_table_holds_row_by_row() {
    let error = TerminalEvent::ResultError {
        errors: vec!["a".into(), "b".into()],
    };
    let exit = TerminalEvent::Exit {
        detail: "claude exited (exit status: 1)".into(),
    };
    let interrupted = Some(TerminalCause::Interrupted);
    let check = Some(TerminalCause::Failed("check: x".into()));
    let rows: Vec<(Option<TerminalCause>, TerminalEvent, TurnOutcome)> = vec![
        (None, success(false), TurnOutcome::Completed),
        (
            None,
            success(true),
            failed("Not logged in · Please run /login"),
        ),
        (None, error.clone(), failed("a; b")),
        (
            None,
            TerminalEvent::ResultError { errors: vec![] },
            failed("claude ended the turn with an error result"),
        ),
        (None, exit.clone(), failed("claude exited (exit status: 1)")),
        (interrupted.clone(), error, TurnOutcome::Interrupted),
        (interrupted.clone(), exit, TurnOutcome::Interrupted),
        (
            interrupted.clone(),
            TerminalEvent::Stopped,
            TurnOutcome::Interrupted,
        ),
        (interrupted.clone(), success(false), TurnOutcome::Completed),
        (interrupted, success(true), TurnOutcome::Interrupted),
        (check.clone(), success(false), failed("check: x")),
        (check, TerminalEvent::Stopped, failed("check: x")),
    ];
    for (cause, event, expected) in rows {
        assert_eq!(
            decide(cause.as_ref(), &event),
            expected,
            "{cause:?} × {event:?}"
        );
    }
}

fn init(thread: Uuid) -> SystemInit {
    SystemInit {
        session_id: thread,
        claude_code_version: "2.1.280".into(),
        model: "claude-haiku-4-5".into(),
        capabilities: vec!["interrupt_receipt_v1".into()],
        mcp_servers: vec![McpStatus {
            name: "neige".into(),
            status: "connected".into(),
        }],
        slash_commands: vec![],
        skills: vec![],
        plugins: vec![PluginRef {
            name: "telemetry".into(),
            source: "telemetry@builtin".into(),
        }],
    }
}

/// One field of an otherwise passing init, changed.
type Deviate = fn(&mut SystemInit);

#[test]
fn every_init_check_refuses_its_own_deviation() {
    let thread = Uuid::new_v4();
    assert_eq!(
        init_check(
            &init(thread),
            &PlannerRole {
                thread,
                version: "2.1.280".into(),
                purpose: SpawnPurpose::Turn
            }
        ),
        Ok(())
    );
    let deviations: Vec<(&str, Deviate)> = vec![
        ("version", |i| i.claude_code_version = "2.1.281".into()),
        ("session", |i| i.session_id = Uuid::new_v4()),
        ("capability", |i| i.capabilities.clear()),
        ("neige", |i| i.mcp_servers[0].status = "failed".into()),
        ("skills", |i| i.skills.push("dataviz".into())),
        ("plugin", |i| i.plugins[0].source = "x@market".into()),
    ];
    for (name, deviate) in deviations {
        let mut record = init(thread);
        deviate(&mut record);
        assert!(
            init_check(
                &record,
                &PlannerRole {
                    thread,
                    version: "2.1.280".into(),
                    purpose: SpawnPurpose::Turn
                }
            )
            .is_err(),
            "{name}"
        );
    }
}

/// A compaction runs with the CLI's commands, so its skills load with them: in place of the turn's
/// no-skills check it needs `compact` listed, and every other check still holds.
#[test]
fn a_compaction_init_needs_the_compact_command_in_place_of_no_skills() {
    let thread = Uuid::new_v4();
    let check = |deviate: Deviate| {
        let mut record = init(thread);
        record.slash_commands = vec!["context".into(), "compact".into()];
        record.skills = vec!["dataviz".into()];
        deviate(&mut record);
        init_check(
            &record,
            &PlannerRole {
                thread,
                version: "2.1.280".into(),
                purpose: SpawnPurpose::Compaction,
            },
        )
    };
    assert_eq!(check(|_| {}), Ok(()));
    assert_eq!(
        check(|i| i.slash_commands.retain(|c| c != "compact")),
        Err("claude lists no /compact command".into())
    );
    let deviations: Vec<(&str, Deviate)> = vec![
        ("version", |i| i.claude_code_version = "2.1.281".into()),
        ("session", |i| i.session_id = Uuid::new_v4()),
        ("capability", |i| i.capabilities.clear()),
        ("neige", |i| i.mcp_servers[0].status = "failed".into()),
        ("plugin", |i| i.plugins[0].source = "x@market".into()),
    ];
    for (name, deviate) in deviations {
        assert!(check(deviate).is_err(), "{name}");
    }
    let mut turn = init(thread);
    turn.slash_commands = vec!["compact".into()];
    turn.skills = vec!["dataviz".into()];
    assert!(
        init_check(
            &turn,
            &PlannerRole {
                thread,
                version: "2.1.280".into(),
                purpose: SpawnPurpose::Turn
            }
        )
        .is_err(),
        "a turn still refuses skills"
    );
}

/// `/compact` that answered without a boundary did not compact: its success fails the compaction,
/// and every other ending is decided as a turn's is.
#[test]
fn a_compaction_success_counts_only_after_its_boundary() {
    let ok = success(false);
    assert_eq!(result_event(ok.clone(), true), ok);
    assert_eq!(
        decide(None, &result_event(ok.clone(), false)),
        failed(NO_BOUNDARY)
    );
    assert_eq!(
        decide(
            Some(&TerminalCause::Interrupted),
            &result_event(ok.clone(), false)
        ),
        TurnOutcome::Interrupted
    );
    for other in [
        success(true),
        TerminalEvent::ResultError { errors: vec![] },
        TerminalEvent::Stopped,
    ] {
        assert_eq!(result_event(other.clone(), false), other);
    }
}

/// The stated invariant: an interrupt's `TurnCompleted` is due by interrupt + `STOP_TIMER` +
/// `SETTLE_AFTER_STOP`, which leaves the run loop at least 5 s of the harness's interrupt budget.
#[test]
fn settle_by_fits_the_harness_interrupt_budget() {
    use crate::claude_planner::session::{SETTLE_AFTER_STOP, STOP_TIMER};
    let budget = crate::harness::HarnessConfig::default().interrupt_completion_budget;
    assert!(
        STOP_TIMER + SETTLE_AFTER_STOP + std::time::Duration::from_secs(5) <= budget,
        "{STOP_TIMER:?} + {SETTLE_AFTER_STOP:?} leaves too little of {budget:?}"
    );
}
