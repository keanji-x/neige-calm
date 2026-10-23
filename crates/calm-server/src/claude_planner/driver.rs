//! The life of one turn after `turn_start` returned `Ok`: read the CLI's stdout, translate, answer
//! its control requests or hand its tool approvals to the harness (#2348), and settle exactly once
//! (design #1791 §5.1 settlement, §6.2).

use std::sync::Arc;
use std::time::Duration;

use crate::provider_execution::{self as execution, stdio::EngineStdio};
use tokio::io::{AsyncBufReadExt as _, BufReader, Lines};
use tokio::sync::watch;
use tokio::time::Instant;
use uuid::Uuid;
use workspace_runtime::execution::*;

use super::approvals::{Approvals, NO_APPROVAL_SURFACE, write_response};
use super::session::Shared;
use super::spawn::{InstructionsFile, SpawnPurpose};
use super::turn_slot::{TerminalCause, TurnSlot, deadline_reached};
use crate::error::CalmError;
use crate::mcp_server::wiring::MCP_SERVER_KEY;
use crate::session_projection_repo::{AgentProvider, ThreadAttribution};
use calm_types::worker::WorkerSessionId;
use provider::claude::permission::{CanUseTool, PermissionDecision, permission_response};
use provider::claude::protocol::{
    ControlResponseOut, ControlResponseOutBody, Record, SystemInit, decode,
};
use provider::claude::translate::{TurnOutcome, TurnTranslator};
use provider::events::{PlannerEvent, PlannerEventKind};

/// When a stop fires, stdout lines already written are still read for at most this long.
const DRAIN_WINDOW: Duration = Duration::from_secs(1);
/// After a result or EOF, the direct child's own exit is awaited this long before `stop`.
const EXIT_WAIT: Duration = Duration::from_secs(5);
/// Own bounds of the settlement's database writes; `settle_by` caps them further once a stop is armed.
const BIND_TIMEOUT: Duration = Duration::from_secs(5);
/// After the child exited, stdout lines it wrote before exiting are still read this long.
const DRAIN_AFTER_EXIT: Duration = Duration::from_secs(1);

pub(crate) struct TurnRun {
    pub(crate) claim: execution::Claim,
    pub(crate) guard: execution::DriverGuard,
    pub(crate) stdout: EngineStdio,
    pub(crate) slot: Arc<TurnSlot>,
    pub(crate) translator: TurnTranslator,
    pub(crate) instructions: InstructionsFile,
    pub(crate) thread: Uuid,
    /// A compaction forwards only its start and completion, counts no usage and records no outcome
    /// (`compact`).
    pub(crate) purpose: SpawnPurpose,
    /// The row's `agent_session_id` is not persisted yet: the first init naming the thread binds
    /// it (a `--session-id` spawn, or a `--resume` spawn after a bind that failed).
    pub(crate) bind: bool,
    /// The pinned `claude_version` this spawn was verified against.
    pub(crate) version: String,
    /// How this spawn's `can_use_tool` requests are answered. Held for the whole read task: its
    /// end, not the child's exit, is the end of the spawn's connection.
    pub(crate) approvals: Approvals,
}

/// The event that ends a turn (§6.2 columns).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TerminalEvent {
    ResultSuccess {
        is_error: bool,
        result: String,
    },
    ResultError {
        errors: Vec<String>,
    },
    /// Stdout ended or the child exited before a result.
    Exit {
        detail: String,
    },
    /// The stop timer, a shutdown, or a failed check or line: the recorded cause decides.
    Stopped,
}

/// §6.2: a recorded cause wins over the event that follows it; otherwise the event decides.
pub(crate) fn decide(cause: Option<&TerminalCause>, event: &TerminalEvent) -> TurnOutcome {
    match (cause, event) {
        (Some(TerminalCause::Failed(check)), _) => TurnOutcome::Failed {
            message: check.clone(),
        },
        (
            Some(TerminalCause::Interrupted),
            TerminalEvent::ResultSuccess {
                is_error: false, ..
            },
        ) => TurnOutcome::Completed,
        (Some(TerminalCause::Interrupted), _) => TurnOutcome::Interrupted,
        (
            None,
            TerminalEvent::ResultSuccess {
                is_error: false, ..
            },
        ) => TurnOutcome::Completed,
        (
            None,
            TerminalEvent::ResultSuccess {
                is_error: true,
                result,
            },
        ) => TurnOutcome::Failed {
            message: result.clone(),
        },
        (None, TerminalEvent::ResultError { errors }) if errors.is_empty() => TurnOutcome::Failed {
            message: "claude ended the turn with an error result".into(),
        },
        (None, TerminalEvent::ResultError { errors }) => TurnOutcome::Failed {
            message: errors.join("; "),
        },
        (None, TerminalEvent::Exit { detail }) => TurnOutcome::Failed {
            message: detail.clone(),
        },
        // Nothing stops a turn without recording why first; keep the reading honest if it did.
        (None, TerminalEvent::Stopped) => TurnOutcome::Failed {
            message: "claude planner turn stopped without a recorded cause".into(),
        },
    }
}

/// The init checks of every spawn (§5.2), the second line of defence after `--version`; a
/// compaction makes its own command check in place of the no-skills one.
pub(crate) fn init_check(
    init: &SystemInit,
    thread: Uuid,
    version: &str,
    purpose: SpawnPurpose,
) -> Result<(), String> {
    if init.claude_code_version != version {
        return Err(format!(
            "claude reports version {}, the config pins {version}",
            init.claude_code_version
        ));
    }
    if init.session_id != thread {
        return Err(format!(
            "claude session {} is not thread {thread}",
            init.session_id
        ));
    }
    if !init
        .capabilities
        .iter()
        .any(|c| c == "interrupt_receipt_v1")
    {
        return Err("claude lacks the interrupt_receipt_v1 capability".into());
    }
    if !init
        .mcp_servers
        .iter()
        .any(|server| server.name == MCP_SERVER_KEY && server.status == "connected")
    {
        return Err("the neige MCP server is not connected".into());
    }
    match purpose {
        SpawnPurpose::Turn if !init.skills.is_empty() => {
            return Err(format!("claude loaded skills {:?}", init.skills));
        }
        SpawnPurpose::Turn => {}
        SpawnPurpose::Compaction => super::compact::init_check(init)?,
    }
    if let Some(plugin) = init
        .plugins
        .iter()
        .find(|p| !p.source.ends_with("@builtin"))
    {
        return Err(format!(
            "claude loaded a non-builtin plugin {}",
            plugin.source
        ));
    }
    Ok(())
}

/// How the read loop ended, which decides whether the child gets its own exit first.
enum Ending {
    /// A result record: close stdin and let the CLI exit on its own.
    Result(TerminalEvent),
    /// Stdout ended or the child exited without a result.
    Exited,
    /// A recorded cause stops the turn now.
    Stopped,
}

pub(crate) async fn drive(shared: Arc<Shared>, run: TurnRun) -> bool {
    let TurnRun {
        claim,
        guard,
        stdout,
        slot,
        mut translator,
        instructions,
        thread,
        purpose,
        bind,
        version,
        approvals,
    } = run;
    let params = &shared.params;
    let engine = params.engine.as_ref().expect("admitted Engine");
    let pool = params.repo.sqlite_db().expect("admitted database");
    let id = execution::execution_id(claim.record()).expect("admitted execution id");
    let mut cancellation = execution::Cancellation {
        owner: params.host.provider_execution.clone(),
        pool: pool.clone(),
        engine: engine.clone(),
        session: params.worker_session_id.clone(),
        claim: claim.clone(),
        armed: true,
    };
    let mut ticks = tokio::time::interval(Duration::from_millis(10));
    let mut lines = BufReader::new(stdout).lines();
    let mut stop_rx = slot.stop_at.subscribe();
    let mut reading = Reading {
        shared: &shared,
        slot: &slot,
        stop_rx: slot.stop_at.subscribe(),
        answering: true,
        translator: &mut translator,
        thread,
        purpose,
        bind,
        version: &version,
        approvals: &approvals,
        compacted: false,
        validated_init: false,
    };
    let mut exited: Option<Instant> = None;
    let ending = loop {
        let drain_until = exited.map(|at| at + DRAIN_AFTER_EXIT);
        // Biased so a stop (the interrupt's timer, or a shutdown) is never starved by a chatty CLI;
        // it first reads, for a bounded window, the lines already written, so a result the CLI
        // finished before the stop still decides the turn (§6.2: finished first ⇒ completed).
        tokio::select! {
            biased;
            _ = deadline_reached(&mut stop_rx, Duration::ZERO) => break reading.drain_then_stop(&mut lines).await,
            line = lines.next_line() => match line {
                Ok(Some(line)) => {
                    if let Some(ending) = reading.on_line(&line).await {
                        break ending;
                    }
                }
                Ok(None) => break Ending::Exited,
                Err(error) => {
                    let ending = reading.protocol_failure(&error);
                    slot.set_stop_reason(execution::StopReason::OutputGap);
                    break ending;
                },
            },
            _ = ticks.tick(), if exited.is_none() => {
                if matches!(engine.output(&id, None).await, Ok(ExecutionReply::Observation {
                    state: ExecutionState::Exited { .. } | ExecutionState::Drained { .. } | ExecutionState::NoChild { .. } | ExecutionState::Revoked, ..
                })) { exited = Some(Instant::now()); }
            }
            _ = sleep_until_opt(drain_until), if drain_until.is_some() => {
                break Ending::Exited;
            }
        }
    };
    let ending = match (purpose, ending) {
        (SpawnPurpose::Compaction, Ending::Result(event)) => {
            Ending::Result(super::compact::result_event(event, reading.compacted))
        }
        (_, ending) => ending,
    };
    let qualified = settle(
        &shared,
        SettleInput {
            claim,
            slot,
            translator,
            instructions,
            thread,
            approvals,
        },
        ending,
    )
    .await;
    cancellation.armed = false;
    drop(guard);
    qualified
}

/// The per-line half of the read loop.
struct Reading<'a> {
    shared: &'a Shared,
    slot: &'a Arc<TurnSlot>,
    /// The slot's stop deadline: a control answer the CLI does not take is abandoned there.
    stop_rx: watch::Receiver<Option<Instant>>,
    /// Off while draining after a stop: stdin closes next, so nothing more is answered.
    answering: bool,
    translator: &'a mut TurnTranslator,
    thread: Uuid,
    purpose: SpawnPurpose,
    /// See [`TurnRun::bind`].
    bind: bool,
    version: &'a str,
    approvals: &'a Approvals,
    /// A `system/compact_boundary` was read.
    compacted: bool,
    /// Only this process's checked session can supply native input/execution evidence.
    validated_init: bool,
}

/// Some CLI records omit session_id; once init qualifies the process they inherit that
/// session. A present field must be a matching UUID, never null or an unrelated session.
#[derive(serde::Deserialize)]
struct DeclaredSession {
    #[serde(default, deserialize_with = "declared_session_id")]
    session_id: Option<Uuid>,
}

fn declared_session_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Uuid>, D::Error> {
    <Uuid as serde::Deserialize>::deserialize(deserializer).map(Some)
}

impl Reading<'_> {
    /// Decode, check, answer and translate one stdout line; `Some` when the line ends the turn.
    async fn on_line(&mut self, line: &str) -> Option<Ending> {
        let record = match decode(line) {
            Ok(record) => record,
            Err(error) => return Some(self.protocol_failure(&error)),
        };
        if !self.validated_init {
            match &record {
                Record::SystemInit(_) => {}
                // Pre-init diagnostics cannot update transcript anchors or produce evidence.
                Record::Ignored { .. } | Record::ControlResponseIn { .. } => return None,
                _ => {
                    return Some(
                        self.protocol_failure(&"native event preceded validated session init"),
                    );
                }
            }
        }
        let session = match serde_json::from_str::<DeclaredSession>(line) {
            Ok(session) => session,
            Err(error) => return Some(self.protocol_failure(&error)),
        };
        if session.session_id.is_some_and(|id| id != self.thread) {
            return Some(self.protocol_failure(&"native event names another session"));
        }
        match &record {
            Record::SystemInit(init) => {
                // The CLI created (or resumed) this thread's session as soon as it names it, even
                // when a later check fails the turn: bind it, so the next spawn resumes it.
                if init.session_id == self.thread {
                    // Publish before the persistence await: init and a durable bind are
                    // independent facts, including when this driver is cancelled mid-bind.
                    let first_init = self.shared.observe_turn(self.slot, |facts| {
                        !std::mem::replace(&mut facts.named_session, true)
                    });
                    if first_init == Some(true) && self.bind {
                        match self
                            .slot
                            .bounded(BIND_TIMEOUT, self.bind_agent_session())
                            .await
                        {
                            Some(persisted) => {
                                self.shared.observe_turn(self.slot, |facts| {
                                    facts.row_bound = persisted;
                                });
                            }
                            None => tracing::warn!(
                                "claude planner: agent session bind cut off by its bound; the next turn retries it"
                            ),
                        }
                    }
                }
                if let Err(check) = init_check(init, self.thread, self.version, self.purpose) {
                    self.slot
                        .record(TerminalCause::Failed(format!("check: {check}")));
                    return Some(Ending::Stopped);
                }
                self.validated_init = true;
            }
            Record::CompactBoundary => self.compacted = true,
            Record::Ignored { kind } => {
                tracing::debug!(kind, "claude planner: ignored stream record");
            }
            Record::ControlRequestIn {
                request_id,
                request,
            } if self.answering => self.on_control_request(request_id, request).await,
            Record::ControlRequestIn { request_id, .. } => {
                tracing::debug!(
                    request_id,
                    "claude planner: control request not answered while stopping"
                );
            }
            Record::ControlCancelRequestIn { request_id } => match self.approvals {
                Approvals::Held(connection) => connection.gone(request_id),
                Approvals::Refused => {
                    tracing::debug!(request_id, "claude planner: control request cancelled");
                }
            },
            _ => {}
        }
        for event in self
            .translator
            .translate_line(line, &record, crate::model::now_ms())
        {
            // A compaction's own usage counts nowhere, like the rest of what it reads (`compact`).
            if self.purpose == SpawnPurpose::Turn {
                if let Some(total) = usage_total(&event) {
                    self.shared.observe_turn(self.slot, |facts| {
                        facts.total_tokens = Some(total);
                    });
                }
                let _ = self.shared.events.send(event);
            } else if matches!(event.kind, PlannerEventKind::ContextReplaced { .. }) {
                let _ = self.shared.events.send(event);
            }
        }
        match record {
            Record::ResultSuccess(success) => Some(Ending::Result(TerminalEvent::ResultSuccess {
                is_error: success.is_error,
                result: success.result,
            })),
            Record::ResultError(error) => Some(Ending::Result(TerminalEvent::ResultError {
                errors: error.errors,
            })),
            _ => None,
        }
    }

    /// Under `ask` a well-formed `can_use_tool` goes to the harness and the read goes on; every other
    /// request is answered here, and a CLI that stopped reading stdin does not hold the turn past
    /// its stop.
    async fn on_control_request(&mut self, request_id: &str, request: &serde_json::Value) {
        let can_use_tool = CanUseTool::from_request(request);
        if let (Approvals::Held(connection), Ok(Some(can_use_tool))) =
            (self.approvals, &can_use_tool)
        {
            connection.open(self.slot, request_id, can_use_tool);
            return;
        }
        tokio::select! {
            biased;
            _ = deadline_reached(&mut self.stop_rx, Duration::ZERO) => {
                tracing::debug!(request_id, "claude planner: control answer abandoned at the stop");
            }
            _ = answer_control_request(self.slot, request_id, can_use_tool.is_ok_and(|request| request.is_none())) => {}
        }
    }

    /// Undecodable output, including bytes that are not UTF-8 and read errors, fails the turn.
    fn protocol_failure(&self, error: &dyn std::fmt::Display) -> Ending {
        tracing::warn!(%error, "claude planner: undecodable stdout");
        self.slot
            .record(TerminalCause::Failed(format!("protocol: {error}")));
        self.slot
            .set_stop_reason(execution::StopReason::SetupFailed);
        Ending::Stopped
    }

    /// Read the lines the CLI already wrote, for at most [`DRAIN_WINDOW`] and without answering
    /// control requests (stdin closes next), then let the recorded cause stop the turn unless one
    /// of those lines already ended it.
    async fn drain_then_stop(&mut self, lines: &mut Lines<BufReader<EngineStdio>>) -> Ending {
        self.answering = false;
        let slot = self.slot;
        // The window is checked on the clock at every line: lines already buffered are returned
        // without yielding, so a CLI that keeps stdout full would otherwise starve any timer
        // raced against this loop.
        let window_ends = Instant::now() + DRAIN_WINDOW;
        let drain = async {
            loop {
                if Instant::now() >= window_ends
                    || slot.settle_by().is_some_and(|at| Instant::now() >= at)
                {
                    return None;
                }
                // Engine reads may require a transport round trip even for queued output.
                // Give them the remaining drain window; the outer bound caps the whole loop.
                match tokio::time::timeout_at(window_ends, lines.next_line()).await {
                    Ok(Ok(Some(line))) => {
                        if let Some(ending) = self.on_line(&line).await {
                            return Some(ending);
                        }
                    }
                    Ok(Ok(None)) | Ok(Err(_)) | Err(_) => return None,
                }
            }
        };
        match slot.bounded(DRAIN_WINDOW, drain).await {
            Some(Some(ending)) => ending,
            Some(None) | None => Ending::Stopped,
        }
    }

    /// Persist `agent_session_id` through the attribution bind, so a session opened for this row
    /// later resumes. A failed write is logged and `false`: this process resumes from memory and
    /// the next turn retries the bind.
    async fn bind_agent_session(&self) -> bool {
        let params = &self.shared.params;
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("c4-bind-session", &params.worker_session_id).await;
        let id = params.worker_session_id.clone();
        let thread = self.thread.to_string();
        let written = crate::db::write_in_tx_typed(params.repo.as_ref(), move |tx| {
            Box::pin(async move {
                // The bind writes `active_turn_id` too; the harness's snapshot owns that column,
                // so the row's current value is passed through unchanged.
                let row = crate::db::sqlite::session_get_tx(tx, &WorkerSessionId(id.clone()))
                    .await?
                    .ok_or_else(|| CalmError::NotFound(format!("worker session {id}")))?;
                crate::db::sqlite::session_bind_attribution_tx(
                    tx,
                    &id,
                    ThreadAttribution {
                        worker_session_id: id.clone(),
                        provider: AgentProvider::Claude,
                        thread_id: Some(thread.clone()),
                        session_id: Some(thread),
                        active_turn_id: row.active_turn_id,
                    },
                )
                .await?;
                Ok(())
            })
        })
        .await;
        match written {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(
                    worker_session_id = %params.worker_session_id,
                    %error,
                    "claude planner: agent session id not persisted; the next turn retries it"
                );
                false
            }
        }
    }
}

/// The lifetime total a usage reading carries.
fn usage_total(event: &PlannerEvent) -> Option<i64> {
    match &event.kind {
        PlannerEventKind::TokenUsage { params } => {
            params["tokenUsage"]["total"]["totalTokens"].as_i64()
        }
        _ => None,
    }
}

async fn sleep_until_opt(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// A `can_use_tool` answered here (one under `never` or `full`, where `--permission-prompts none`
/// should not send it, or one without its recorded shape) is denied. Any other request gets an error response.
async fn answer_control_request(slot: &TurnSlot, request_id: &str, other_subtype: bool) {
    let response = if other_subtype {
        ControlResponseOut::new(ControlResponseOutBody::Error {
            request_id: request_id.to_string(),
            error: "unsupported control request".into(),
        })
    } else {
        permission_response(
            request_id,
            &PermissionDecision::Deny {
                message: NO_APPROVAL_SURFACE.into(),
            },
        )
    };
    if let Err(error) = write_response(slot, &response).await {
        tracing::warn!(%error, request_id, "claude planner: control response not written");
    }
}

struct SettleInput {
    claim: execution::Claim,
    slot: Arc<TurnSlot>,
    translator: TurnTranslator,
    instructions: InstructionsFile,
    thread: Uuid,
    approvals: Approvals,
}

/// Every protocol end requests Stop. Only Drained settlement qualifies a known result.
async fn settle(shared: &Arc<Shared>, input: SettleInput, ending: Ending) -> bool {
    let SettleInput {
        claim,
        slot,
        translator,
        instructions,
        thread,
        approvals,
    } = input;
    let params = &shared.params;
    let owner = &params.host.provider_execution;
    let engine = params.engine.as_ref().expect("admitted Engine");
    let pool = params.repo.sqlite_db().expect("admitted database");
    let id = execution::execution_id(claim.record()).expect("admitted id");
    let event = match ending {
        Ending::Result(event) => event,
        Ending::Exited => TerminalEvent::Exit {
            detail: "provider output ended".into(),
        },
        Ending::Stopped => TerminalEvent::Stopped,
    };
    // Freeze the facts observed at protocol end; cleanup must not turn a later
    // control-write failure or interrupt into an earlier terminal cause.
    let cause = slot.cause();
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point("c4-protocol-ended", &params.worker_session_id).await;
    if let Some(mut stdin) = slot
        .bounded(Duration::from_secs(5), slot.stdin.lock())
        .await
    {
        stdin.take();
    }
    let _ = slot
        .bounded(Duration::from_secs(5), engine.close_stdin(&id))
        .await;
    if matches!(
        event,
        TerminalEvent::ResultSuccess { .. } | TerminalEvent::ResultError { .. }
    ) && !slot.stop_armed()
    {
        let _ = slot
            .bounded(EXIT_WAIT, async {
                loop {
                    if matches!(
                        engine.output(&id, None).await,
                        Ok(ExecutionReply::Observation {
                            state: ExecutionState::Exited { .. }
                                | ExecutionState::Drained { .. }
                                | ExecutionState::NoChild { .. }
                                | ExecutionState::Revoked,
                            ..
                        })
                    ) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await;
    }
    if let Err(error) = owner
        .stop(
            &pool,
            Some(engine),
            &params.worker_session_id,
            &claim,
            slot.stop_reason(),
        )
        .await
    {
        tracing::warn!(%error, "Claude Stop retained claim");
    }
    let _ = slot
        .bounded(
            super::session::SETTLE_AFTER_STOP,
            execution::wait_settled(engine, &id, super::session::SETTLE_AFTER_STOP),
        )
        .await;
    let settlement = match execution::settlement(&pool, &id).await {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "Claude settlement read failed; completion is unconfirmed");
            None
        }
    };
    let qualified = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let turn = match &claim.record().purpose {
        execution::Purpose::Turn { turn_id } | execution::Purpose::Compaction { turn_id } => {
            turn_id.clone()
        }
        _ => unreachable!(),
    };
    let known = Some(execution::KnownEnd::ClaudeTurn(Box::new(
        super::execution::End {
            shared: shared.clone(),
            slot,
            translator,
            instructions,
            event,
            cause,
            qualified: qualified.clone(),
            thread: thread.to_string(),
            turn,
            approvals,
        },
    )));
    let result = if settlement.is_some() {
        owner
            .finish(
                &pool,
                Some(engine),
                &params.worker_session_id,
                &claim,
                known,
            )
            .await
    } else {
        owner
            .owner
            .complete(
                &params.worker_session_id,
                claim.record(),
                execution::CompletionEvidence::Unconfirmed,
                known,
            )
            .await
    };
    if let Err(error) = result {
        tracing::warn!(%error, "Claude completion retained claim");
    }
    qualified.load(std::sync::atomic::Ordering::Acquire)
}
