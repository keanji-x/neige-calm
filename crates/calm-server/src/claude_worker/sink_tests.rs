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
    inits: Mutex<Vec<(WorkerTurnKey, WorkerInitRecord)>>,
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

/// Drive one worker turn; with `interrupt`, interrupt it once its init is recorded.
async fn run(
    ledger: Ledger,
    init_overrides: Value,
    files: impl FnOnce(&Path, Uuid),
    script: &str,
    interrupt: bool,
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
    let sink = WorkerSink {
        slot: slot.clone(),
        owner: owner.clone(),
        pool: pool.clone(),
        engine: (*fixture.engine).clone(),
        turn: key.clone(),
        thread,
        ledger: ledger.clone(),
    };
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
        if interrupt {
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
        false,
    )
    .await;
    assert!(ran.qualified);
    expect_init(&ran);
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
        true,
    )
    .await;
    assert!(ran.qualified);
    expect_init(&ran);
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
        false,
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
        false,
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
        false,
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
