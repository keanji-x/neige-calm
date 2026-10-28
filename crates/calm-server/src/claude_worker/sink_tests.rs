//! The worker sink through the real protocol loop, Engine and supervisor: a non-Planner sink's
//! init, record and known_end on a result and on an interrupt (WA1b adversarial review), and the
//! durable contract of its turn port.
use super::sink::NO_APPROVER;
use super::{
    WorkerInitRecord, WorkerRole, WorkerSink, WorkerTurnKey, WorkerTurnLedger, WorkerTurnOutcome,
    WorkerTurnRecord,
};
use crate::claude_planner::protocol_tests::{Admitted, SESSION, admitted};
use crate::claude_planner::protocol_turn::{ProtocolTurn, drive};
use crate::claude_planner::turn_slot::{TerminalCause, TurnSlot};
use crate::error::{CalmError, Result};
use crate::provider_execution::{self as execution, stdio::EngineStdio};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

#[derive(Default)]
struct Ledger {
    refuse_init: bool,
    refuse_outcome: bool,
    refuse_result: bool,
    inits: Mutex<Vec<(WorkerTurnKey, WorkerInitRecord)>>,
    results: Mutex<Vec<(WorkerTurnKey, WorkerTurnOutcome)>>,
    outcomes: Mutex<Vec<(WorkerTurnKey, WorkerTurnRecord)>>,
}
#[async_trait::async_trait]
impl WorkerTurnLedger for Ledger {
    async fn record_init(&self, turn: &WorkerTurnKey, init: &WorkerInitRecord) -> Result<()> {
        if self.refuse_init {
            return Err(CalmError::Conflict("ledger unavailable".into()));
        }
        self.inits
            .lock()
            .unwrap()
            .push((turn.clone(), init.clone()));
        Ok(())
    }
    async fn record_result(&self, turn: &WorkerTurnKey, result: &WorkerTurnOutcome) -> Result<()> {
        if self.refuse_result {
            return Err(CalmError::Conflict("ledger unavailable".into()));
        }
        self.results
            .lock()
            .unwrap()
            .push((turn.clone(), result.clone()));
        Ok(())
    }
    async fn record_outcome(&self, turn: &WorkerTurnKey, record: &WorkerTurnRecord) -> Result<()> {
        if self.refuse_outcome {
            return Err(CalmError::Conflict("ledger unavailable".into()));
        }
        self.outcomes
            .lock()
            .unwrap()
            .push((turn.clone(), record.clone()));
        Ok(())
    }
}

/// The worker owner's completion step alone: the sink's end records itself. It borrows a
/// consumer the fixture Engine projects; the worker owner's own registration is WA2-D's.
struct WorkerOwner;
#[async_trait::async_trait]
impl execution::ProviderExecutionOwner for WorkerOwner {
    fn consumer(&self) -> &'static str {
        crate::claude_planner::execution::CONSUMER
    }
    fn alias_prefix(&self) -> execution::AliasPrefix {
        execution::AliasPrefix::planner()
    }
    fn alias(&self, session: &str, id: &str) -> String {
        self.alias_prefix().alias(session, id)
    }
    async fn complete(
        &self,
        session: &str,
        record: &execution::Record,
        evidence: execution::CompletionEvidence,
        known: Option<execution::KnownEnd>,
    ) -> Result<()> {
        match known {
            Some(execution::KnownEnd::ClaudeWorkerTurn(end)) => {
                end.settle(session, record, &evidence).await
            }
            _ => Err(execution::conflict("not a worker turn")),
        }
    }
}

fn init(thread: Uuid, overrides: Value) -> Value {
    let mut init = json!({"type":"system","subtype":"init","session_id":thread,
        "claude_code_version":"2.1.280","model":"test","capabilities":["interrupt_receipt_v1"],
        "mcp_servers":[{"name":"neige","status":"connected"}],"slash_commands":["review"],
        "skills":["navi"],"plugins":[{"name":"finance","source":"finance@synced"}]});
    for (key, value) in overrides.as_object().unwrap() {
        init[key] = value.clone();
    }
    init
}

fn result(thread: Uuid) -> Value {
    json!({"type":"result","subtype":"success","session_id":thread,"is_error":false,
        "result":"done","usage":{"input_tokens":0,"cache_creation_input_tokens":0,
        "cache_read_input_tokens":0,"output_tokens":0,"iterations":[]},"modelUsage":{}})
}

fn write(dir: &Path, name: &str, lines: &[Value]) {
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    std::fs::write(dir.join(name), text).unwrap();
}

struct Ran {
    ledger: Arc<Ledger>,
    key: WorkerTurnKey,
    thread: Uuid,
    qualified: bool,
    dir: tempfile::TempDir,
}

/// How the test treats the turn it drives.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Then {
    /// Let the shared loop run to its end.
    Finish,
    /// Interrupt the turn once its init is recorded.
    Interrupt,
    /// Drop the shared loop, as a kernel death would, once the child has seen stdin close: the
    /// loop read the result, but no known end or completion exists yet.
    DieAfterResult,
}

/// Drive one worker turn.
async fn run(
    ledger: Ledger,
    init_overrides: Value,
    files: impl FnOnce(&Path, Uuid),
    script: &str,
    then: Then,
) -> Ran {
    let ledger = Arc::new(ledger);
    let owner = execution::ProviderExecution {
        owner: Arc::new(WorkerOwner),
        drivers: Default::default(),
    };
    let Admitted {
        repo: _repo,
        pool,
        fixture,
        dir,
        thread,
        id,
        claim,
        guard,
    } = admitted(
        &owner,
        |dir, thread| {
            write(dir, "init.json", &[init(thread, init_overrides)]);
            write(dir, "result.json", &[result(thread)]);
            files(dir, thread);
        },
        script,
    )
    .await;
    let key = WorkerTurnKey {
        worker_session_id: SESSION.into(),
        intent_key: format!("claude-worker-turn:{SESSION}:first"),
        execution_id: id.as_str().into(),
    };
    let slot = Arc::new(TurnSlot::new(
        EngineStdio::new((*fixture.engine).clone(), id.clone()),
        Duration::from_secs(12),
    ));
    let sink = WorkerSink::new(
        slot.clone(),
        owner.clone(),
        pool.clone(),
        (*fixture.engine).clone(),
        key.clone(),
        ledger.clone(),
    );
    let drive = drive(
        sink,
        ProtocolTurn {
            claim,
            guard,
            stdout: EngineStdio::new((*fixture.engine).clone(), id.clone()),
            profile: WorkerRole { thread },
        },
    );
    let stop = async {
        if then == Then::Interrupt {
            tokio::time::timeout(Duration::from_secs(10), async {
                while ledger.inits.lock().unwrap().is_empty() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            slot.record(TerminalCause::Interrupted);
            slot.arm_stop(tokio::time::Instant::now());
        }
    };
    if then == Then::DieAfterResult {
        drop(stop);
        let closed = dir.path().join("stdin-closed");
        tokio::select! {
            _ = drive => panic!("the turn ended before the kernel died"),
            () = async {
                while !closed.exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            } => {}
            () = tokio::time::sleep(Duration::from_secs(10)) => panic!("stdin never closed"),
        }
        // The loop is gone with the child still waiting to exit: no known end was packaged and no
        // completion ran, so only what the sink wrote while reading survives.
        assert!(ledger.outcomes.lock().unwrap().is_empty());
        // The dropped loop's cancellation stops the child; let it settle before the Engine goes.
        execution::wait_settled(&fixture.engine, &id, Duration::from_secs(12))
            .await
            .unwrap();
        return Ran {
            ledger,
            key,
            thread,
            qualified: false,
            dir,
        };
    }
    let (qualified, ()) =
        tokio::time::timeout(Duration::from_secs(20), async { tokio::join!(drive, stop) })
            .await
            .unwrap();
    // The protocol loop owned the execution end to end: stdin closed, driver gone, claim cleared.
    assert!(slot.stdin.lock().await.is_none());
    assert!(*slot.settled.borrow());
    assert!(!owner.drivers.contains(&id));
    assert!(
        calm_truth::db::sqlite::provider_execution::load(pool.read_pool(), SESSION)
            .await
            .unwrap()
            .is_none()
    );
    Ran {
        ledger,
        key,
        thread,
        qualified,
        dir,
    }
}

fn recorded_init(ran: &Ran) -> Vec<(WorkerTurnKey, WorkerInitRecord)> {
    ran.ledger.inits.lock().unwrap().clone()
}

fn results(ran: &Ran) -> Vec<(WorkerTurnKey, WorkerTurnOutcome)> {
    ran.ledger.results.lock().unwrap().clone()
}

fn expect_result(ran: &Ran, outcome_now: WorkerTurnOutcome) {
    assert_eq!(results(ran), [(ran.key.clone(), outcome_now)]);
}

fn outcome(ran: &Ran) -> Vec<(WorkerTurnKey, WorkerTurnRecord)> {
    ran.ledger.outcomes.lock().unwrap().clone()
}

fn expect_init(ran: &Ran) {
    assert_eq!(
        recorded_init(ran),
        [(
            ran.key.clone(),
            WorkerInitRecord {
                session: ran.thread,
                claude_code_version: "2.1.280".into(),
            }
        )]
    );
}

fn expect_outcome(ran: &Ran, outcome_now: WorkerTurnOutcome) {
    assert_eq!(
        outcome(ran),
        [(
            ran.key.clone(),
            WorkerTurnRecord {
                outcome: outcome_now,
                settled: true,
            }
        )]
    );
}

#[tokio::test]
async fn w_protocol_minimal_sink_worker_result() {
    let ran = run(
        Ledger::default(),
        json!({}),
        |_, _| {},
        "cat init.json result.json",
        Then::Finish,
    )
    .await;
    assert!(ran.qualified);
    expect_init(&ran);
    expect_result(&ran, WorkerTurnOutcome::Completed);
    expect_outcome(&ran, WorkerTurnOutcome::Completed);
    // A later turn resumes exactly the session the recorded init named.
    assert_eq!(recorded_init(&ran)[0].1.resume().thread(), ran.thread);
}

#[tokio::test]
async fn w_protocol_minimal_sink_worker_interrupt() {
    let ran = run(
        Ledger::default(),
        json!({}),
        |_, _| {},
        "cat init.json; exec sleep 120",
        Then::Interrupt,
    )
    .await;
    assert!(ran.qualified);
    expect_init(&ran);
    assert!(results(&ran).is_empty());
    expect_outcome(&ran, WorkerTurnOutcome::Interrupted);
}

#[tokio::test]
async fn worker_sink_denies_permission_request_back_to_the_model() {
    let ran = run(
        Ledger::default(),
        json!({}),
        |dir, _| {
            write(
                dir,
                "control.json",
                &[json!({"type":"control_request","request_id":"req-1",
                    "request":{"subtype":"can_use_tool","tool_name":"Bash",
                    "input":{"command":"rm -rf build"}}})],
            )
        },
        "cat init.json control.json; IFS= read -r reply; printf '%s\\n' \"$reply\" > reply.json; cat result.json",
        Then::Finish,
    )
    .await;
    let reply: Value =
        serde_json::from_str(&std::fs::read_to_string(ran.dir.path().join("reply.json")).unwrap())
            .unwrap();
    assert_eq!(
        reply,
        json!({"type":"control_response","response":{"subtype":"success","request_id":"req-1",
            "response":{"behavior":"deny","message":NO_APPROVER}}})
    );
    expect_outcome(&ran, WorkerTurnOutcome::Completed);
}

#[tokio::test]
async fn worker_sink_records_init_before_the_check() {
    let ran = run(
        Ledger::default(),
        json!({"mcp_servers": []}),
        |_, _| {},
        "cat init.json; exec sleep 120",
        Then::Finish,
    )
    .await;
    // The CLI named the session, so the next turn may resume it, though this one failed its check.
    expect_init(&ran);
    expect_outcome(
        &ran,
        WorkerTurnOutcome::Failed {
            message: "check: the neige MCP server is not connected".into(),
        },
    );
}

#[tokio::test]
async fn worker_sink_unrecorded_init_stops_the_turn() {
    let ran = run(
        Ledger {
            refuse_init: true,
            ..Ledger::default()
        },
        json!({}),
        |_, _| {},
        "cat init.json; exec sleep 120",
        Then::Finish,
    )
    .await;
    assert!(recorded_init(&ran).is_empty());
    let outcomes = outcome(&ran);
    assert_eq!(outcomes.len(), 1);
    assert!(
        matches!(&outcomes[0].1.outcome, WorkerTurnOutcome::Failed { message } if message.starts_with("init not recorded")),
        "{outcomes:?}"
    );
}

/// Exercise the completion port without a process: persistence failure and identity checks
/// must leave completion unqualified so the owner retains the claim for recovery.
#[tokio::test]
async fn worker_sink_completion_requires_matching_identity_and_a_durable_write() {
    use super::sink::WorkerEnd;
    use calm_truth::db::sqlite::provider_execution::{Decision, Purpose, Release, Version};
    use std::sync::atomic::{AtomicBool, Ordering};

    let record = execution::Record {
        version: Version::V1,
        execution_id: "execution-1".into(),
        purpose: Purpose::RewindCheck,
        release: Release::Held,
        decision: Decision::Observing,
    };
    for (session, execution_id, refuse_outcome) in [
        ("other-session", "execution-1", false),
        (SESSION, "other-execution", false),
        (SESSION, "execution-1", true),
        (SESSION, "execution-1", false),
    ] {
        let ledger = Arc::new(Ledger {
            refuse_outcome,
            ..Ledger::default()
        });
        let slot = Arc::new(TurnSlot::new(tokio::io::sink(), Duration::from_secs(12)));
        let qualified = Arc::new(AtomicBool::new(false));
        let turn = WorkerTurnKey {
            worker_session_id: SESSION.into(),
            intent_key: "turn-1".into(),
            execution_id: execution_id.into(),
        };
        let end = WorkerEnd {
            turn: turn.clone(),
            outcome: WorkerTurnOutcome::Completed,
            result_durable: true,
            slot: slot.clone(),
            ledger: ledger.clone(),
            qualified: qualified.clone(),
        };
        let result = end
            .settle(
                session,
                &record,
                &execution::CompletionEvidence::Unconfirmed,
            )
            .await;
        if session != SESSION || execution_id != record.execution_id || refuse_outcome {
            assert!(matches!(result, Err(CalmError::Conflict(_))));
            assert!(ledger.outcomes.lock().unwrap().is_empty());
            assert!(!*slot.settled.borrow());
        } else {
            result.unwrap();
            assert_eq!(
                *ledger.outcomes.lock().unwrap(),
                [(
                    turn,
                    WorkerTurnRecord {
                        outcome: WorkerTurnOutcome::Completed,
                        settled: false,
                    }
                )]
            );
            assert!(*slot.settled.borrow());
        }
        assert!(!qualified.load(Ordering::Acquire));
    }
}

/// The result fact is written while the loop reads the result, so a kernel that dies before the
/// known end exists (§3.6: after `result`, before `complete`) still leaves it for recovery.
#[tokio::test]
async fn w_protocol_worker_result_recorded_before_known_end() {
    for (record, expected) in [
        (None, WorkerTurnOutcome::Completed),
        (
            Some(
                json!({"type":"result","subtype":"error_during_execution","is_error":true,
                "errors":["tool crashed"],"usage":{"input_tokens":0,
                "cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":0,
                "iterations":[]},"modelUsage":{}}),
            ),
            WorkerTurnOutcome::Failed {
                message: "tool crashed".into(),
            },
        ),
    ] {
        let ran = run(
            Ledger::default(),
            json!({}),
            |dir, thread| {
                if let Some(mut record) = record {
                    record["session_id"] = json!(thread);
                    write(dir, "result.json", &[record]);
                }
            },
            // The child outlives the result: it marks stdin's close, then never exits.
            "cat init.json result.json; cat >/dev/null; : > stdin-closed; exec sleep 120",
            Then::DieAfterResult,
        )
        .await;
        expect_init(&ran);
        expect_result(&ran, expected);
    }
}

/// A result the port cannot record stops the turn, which then neither completes nor qualifies.
#[tokio::test]
async fn w_protocol_worker_unrecorded_result_never_completes() {
    let ran = run(
        Ledger {
            refuse_result: true,
            ..Ledger::default()
        },
        json!({}),
        |_, _| {},
        "cat init.json result.json; exec sleep 120",
        Then::Finish,
    )
    .await;
    assert!(!ran.qualified);
    assert!(results(&ran).is_empty());
    let outcomes = outcome(&ran);
    assert_eq!(outcomes.len(), 1);
    assert!(
        matches!(&outcomes[0].1.outcome, WorkerTurnOutcome::Failed { message } if message.starts_with("result not recorded")),
        "{outcomes:?}"
    );
}

/// The profile's thread is the turn's only session: an init naming another one fails the turn
/// before the sink records anything.
#[tokio::test]
async fn w_protocol_worker_init_for_another_session_fails() {
    let ran = run(
        Ledger::default(),
        json!({"session_id": Uuid::new_v4()}),
        |_, _| {},
        "cat init.json result.json",
        Then::Finish,
    )
    .await;
    assert!(recorded_init(&ran).is_empty());
    assert!(results(&ran).is_empty());
    let outcomes = outcome(&ran);
    assert_eq!(outcomes.len(), 1);
    assert!(
        matches!(&outcomes[0].1.outcome, WorkerTurnOutcome::Failed { message } if message.starts_with("protocol:")),
        "{outcomes:?}"
    );
}
