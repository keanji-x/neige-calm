//! The life of one turn after `turn_start` returned `Ok`: read the CLI's stdout, translate, answer
//! its control requests, and settle exactly once (design #1791 §5.1 settlement, §6.2).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncBufReadExt as _, BufReader, Lines};
use tokio::process::{Child, ChildStderr, ChildStdout};
use tokio::sync::watch;
use tokio::time::Instant;
use uuid::Uuid;

use super::protocol::{ControlResponseOut, ControlResponseOutBody, Record, SystemInit, decode};
use super::session::{Shared, TerminalCause, TurnSlot, WRITE_TIMEOUT, deadline_reached};
use super::spawn::{InstructionsFile, SessionStart};
use super::stop::{STOP_BOUND, stop_by};
use super::translate::{TurnOutcome, TurnTranslator};
use crate::codex_appserver::Notification;
use crate::error::CalmError;
use crate::session_projection_repo::{AgentProvider, ThreadAttribution};
use calm_types::worker::WorkerSessionId;

/// When a stop fires, stdout lines already written are still read for at most this long.
const DRAIN_WINDOW: Duration = Duration::from_secs(1);
/// After a result or EOF, the direct child's own exit is awaited this long before `stop`.
const EXIT_WAIT: Duration = Duration::from_secs(5);
/// Own bounds of the settlement's database writes; `settle_by` caps them further once a stop is armed.
const RECORD_TIMEOUT: Duration = Duration::from_secs(5);
const BIND_TIMEOUT: Duration = Duration::from_secs(5);
/// After the final `start_kill`, the direct child is reaped for at most this long.
const REAP_WAIT: Duration = Duration::from_secs(1);
/// After the child exited, stdout lines it wrote before exiting are still read this long.
const DRAIN_AFTER_EXIT: Duration = Duration::from_secs(1);

pub(crate) struct TurnRun {
    pub(crate) child: Child,
    pub(crate) stdout: ChildStdout,
    pub(crate) stderr: ChildStderr,
    pub(crate) slot: Arc<TurnSlot>,
    pub(crate) translator: TurnTranslator,
    pub(crate) instructions: InstructionsFile,
    pub(crate) thread: Uuid,
    /// How this spawn named the Claude session; `New` binds `agent_session_id` on the first init.
    pub(crate) start: SessionStart,
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

/// The init checks of every spawn (§5.2), the second line of defence after `--version`.
pub(crate) fn init_check(init: &SystemInit, thread: Uuid, version: &str) -> Result<(), String> {
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
        .any(|server| server.name == "calm" && server.status == "connected")
    {
        return Err("the calm MCP server is not connected".into());
    }
    if !init.skills.is_empty() {
        return Err(format!("claude loaded skills {:?}", init.skills));
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
    Exited(Option<std::process::ExitStatus>),
    /// A recorded cause stops the turn now.
    Stopped,
}

pub(crate) async fn drive(shared: Arc<Shared>, run: TurnRun) {
    let TurnRun {
        mut child,
        stdout,
        stderr,
        slot,
        mut translator,
        instructions,
        thread,
        start,
    } = run;
    let stderr_tail = Arc::new(Mutex::new(String::new()));
    let stderr_task = tokio::spawn(keep_stderr_tail(stderr, Arc::clone(&stderr_tail)));
    let mut lines = BufReader::new(stdout).lines();
    let mut stop_rx = slot.stop_at.subscribe();
    let mut reading = Reading {
        shared: &shared,
        slot: &slot,
        stop_rx: slot.stop_at.subscribe(),
        answering: true,
        translator: &mut translator,
        thread,
        start,
        bound: false,
        total_tokens: None,
    };
    let mut exited: Option<(std::process::ExitStatus, Instant)> = None;
    let ending = loop {
        let drain_until = exited.map(|(_, at)| at + DRAIN_AFTER_EXIT);
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
                Ok(None) => break Ending::Exited(exited.map(|(status, _)| status)),
                Err(error) => break reading.protocol_failure(&error),
            },
            status = child.wait(), if exited.is_none() => {
                // Output written just before the exit is still read, for at most the drain bound.
                match status.ok() {
                    Some(status) => exited = Some((status, Instant::now())),
                    None => break Ending::Exited(None),
                }
            }
            _ = sleep_until_opt(drain_until), if drain_until.is_some() => {
                break Ending::Exited(exited.map(|(status, _)| status));
            }
        }
    };
    let (bound, total_tokens) = (reading.bound, reading.total_tokens);
    settle(
        &shared,
        SettleInput {
            child,
            slot,
            translator,
            instructions,
            thread,
            bound,
            total_tokens,
            stderr_tail,
        },
        ending,
    )
    .await;
    stderr_task.abort();
}

/// The per-line half of the read loop.
struct Reading<'a> {
    shared: &'a Shared,
    slot: &'a TurnSlot,
    /// The slot's stop deadline: a control answer the CLI does not take is abandoned there.
    stop_rx: watch::Receiver<Option<Instant>>,
    /// Off while draining after a stop: stdin closes next, so nothing more is answered.
    answering: bool,
    translator: &'a mut TurnTranslator,
    thread: Uuid,
    start: SessionStart,
    /// A valid `system/init` named this thread's session.
    bound: bool,
    total_tokens: Option<i64>,
}

impl Reading<'_> {
    /// Decode, check, answer and translate one stdout line; `Some` when the line ends the turn.
    async fn on_line(&mut self, line: &str) -> Option<Ending> {
        let record = match decode(line) {
            Ok(record) => record,
            Err(error) => return Some(self.protocol_failure(&error)),
        };
        match &record {
            Record::SystemInit(init) => {
                // The CLI created (or resumed) this thread's session as soon as it names it, even
                // when a later check fails the turn: bind it, so the next spawn resumes it.
                if init.session_id == self.thread {
                    if !self.bound && self.start == SessionStart::New {
                        let bound = self
                            .slot
                            .bounded(BIND_TIMEOUT, self.bind_agent_session())
                            .await;
                        if bound.is_none() {
                            tracing::warn!(
                                "claude planner: agent session bind cut off by its bound"
                            );
                        }
                    }
                    self.bound = true;
                }
                let version = &self.shared.params.host.config.claude_version;
                if let Err(check) = init_check(init, self.thread, version) {
                    self.slot
                        .record(TerminalCause::Failed(format!("check: {check}")));
                    return Some(Ending::Stopped);
                }
            }
            Record::Ignored { kind } => {
                tracing::debug!(kind, "claude planner: ignored stream record");
            }
            Record::ControlRequestIn {
                request_id,
                request,
            } if self.answering => {
                // A CLI that stopped reading stdin must not hold the turn past its stop.
                tokio::select! {
                    biased;
                    _ = deadline_reached(&mut self.stop_rx, Duration::ZERO) => {
                        tracing::debug!(request_id, "claude planner: control answer abandoned at the stop");
                    }
                    _ = answer_control_request(self.slot, request_id, request) => {}
                }
            }
            Record::ControlRequestIn { request_id, .. } => {
                tracing::debug!(
                    request_id,
                    "claude planner: control request not answered while stopping"
                );
            }
            _ => {}
        }
        for notification in self.translator.translate(&record, crate::model::now_ms()) {
            self.total_tokens = usage_total(&notification).or(self.total_tokens);
            let _ = self.shared.notifications.send(notification);
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

    /// Undecodable output, including bytes that are not UTF-8 and read errors, fails the turn.
    fn protocol_failure(&self, error: &dyn std::fmt::Display) -> Ending {
        tracing::warn!(%error, "claude planner: undecodable stdout");
        self.slot
            .record(TerminalCause::Failed(format!("protocol: {error}")));
        Ending::Stopped
    }

    /// Read the lines the CLI already wrote, for at most [`DRAIN_WINDOW`] and without answering
    /// control requests (stdin closes next), then let the recorded cause stop the turn unless one
    /// of those lines already ended it.
    async fn drain_then_stop(&mut self, lines: &mut Lines<BufReader<ChildStdout>>) -> Ending {
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
                // `next_line` is cancel-safe; the zero timeout gives up as soon as no complete
                // line is ready (tokio may poll it again within the next timer tick).
                match tokio::time::timeout(Duration::ZERO, lines.next_line()).await {
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
    /// later resumes. A failed write is logged: this process still resumes from memory.
    async fn bind_agent_session(&self) {
        let params = &self.shared.params;
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
        if let Err(error) = written {
            tracing::warn!(
                worker_session_id = %params.worker_session_id,
                %error,
                "claude planner: agent session id not persisted"
            );
        }
    }
}

/// The lifetime total a `thread/tokenUsage/updated` frame carries.
fn usage_total(notification: &Notification) -> Option<i64> {
    match notification {
        Notification::Other { method, params } if method == "thread/tokenUsage/updated" => {
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

async fn keep_stderr_tail(stderr: ChildStderr, tail: Arc<Mutex<String>>) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::debug!(line = %line, "claude planner stderr");
        if !line.trim().is_empty() {
            *tail.lock().expect("stderr tail poisoned") = line;
        }
    }
}

/// `can_use_tool` is not expected with `--permission-prompts none`; it is denied. Anything else
/// gets an error response.
async fn answer_control_request(slot: &TurnSlot, request_id: &str, request: &serde_json::Value) {
    let body = if request.get("subtype").and_then(|s| s.as_str()) == Some("can_use_tool") {
        ControlResponseOutBody::Success {
            request_id: request_id.to_string(),
            response: json!({
                "behavior": "deny",
                "message": "this Planner has no approval surface",
            }),
        }
    } else {
        ControlResponseOutBody::Error {
            request_id: request_id.to_string(),
            error: "unsupported control request".into(),
        }
    };
    let written = match serde_json::to_string(&ControlResponseOut::new(body)) {
        Ok(line) => slot.write_line(&line).await,
        Err(error) => Err(error.into()),
    };
    if let Err(error) = written {
        tracing::warn!(%error, request_id, "claude planner: control response not written");
    }
}

struct SettleInput {
    child: Child,
    slot: Arc<TurnSlot>,
    translator: TurnTranslator,
    instructions: InstructionsFile,
    thread: Uuid,
    bound: bool,
    total_tokens: Option<i64>,
    stderr_tail: Arc<Mutex<String>>,
}

/// record → close stdin → bounded wait for the direct child (not on the stop path) → `stop` →
/// remove the instructions file → close open items → `TurnCompleted`.
/// Every await here is bounded by its own timeout and, once a stop is armed, by the slot's
/// `settle_by`, so `TurnCompleted` goes out by then whatever the CLI does. A bound that passes
/// is logged and settlement moves on: an unrecorded outcome is left to boot recovery, and a stop
/// that did not confirm is left to the next spawn's stop, which fails closed (§5.1).
async fn settle(shared: &Shared, input: SettleInput, ending: Ending) {
    let SettleInput {
        mut child,
        slot,
        mut translator,
        instructions,
        thread,
        bound,
        total_tokens,
        stderr_tail,
    } = input;
    let params = &shared.params;
    // A result read while the turn was already stopping gets no graceful exit wait (D29).
    let (event, wait_for_exit) = match ending {
        Ending::Result(event) => (event, !slot.stop_armed()),
        Ending::Exited(status) => {
            let status = match status {
                Some(status) => Some(status),
                None => slot
                    .bounded(EXIT_WAIT, child.wait())
                    .await
                    .and_then(|status| status.ok()),
            };
            let tail = stderr_tail.lock().expect("stderr tail poisoned").clone();
            let mut detail = match status {
                Some(status) => format!("claude exited ({status})"),
                None => "claude closed its output and did not exit".to_string(),
            };
            if !tail.is_empty() {
                detail.push_str(": ");
                detail.push_str(&tail);
            }
            (TerminalEvent::Exit { detail }, false)
        }
        Ending::Stopped => (TerminalEvent::Stopped, false),
    };
    let outcome = decide(slot.cause().as_ref(), &event);
    let completed = translator.turn_completed(&outcome);
    let Notification::TurnCompleted { turn, .. } = &completed else {
        unreachable!("turn_completed builds TurnCompleted");
    };
    let turn_id = turn["id"].as_str().unwrap_or_default().to_string();
    let recorded = slot
        .bounded(
            RECORD_TIMEOUT,
            crate::harness::turn_outcome::record(
                params.repo.as_ref(),
                &params.worker_session_id,
                &params.card_id,
                &params.track_id,
                &thread.to_string(),
                &turn_id,
                turn,
            ),
        )
        .await;
    match recorded {
        Some(Ok(_)) => {}
        Some(Err(error)) => tracing::warn!(
            worker_session_id = %params.worker_session_id,
            turn_id,
            %error,
            "claude planner: turn outcome not recorded"
        ),
        None => tracing::warn!(
            worker_session_id = %params.worker_session_id,
            turn_id,
            "claude planner: turn outcome not recorded by its bound; boot recovery records it"
        ),
    }
    if let Some(mut stdin) = slot.bounded(WRITE_TIMEOUT, slot.stdin.lock()).await {
        stdin.take();
    }
    if wait_for_exit && slot.bounded(EXIT_WAIT, child.wait()).await.is_none() {
        tracing::debug!(turn_id, "claude planner: the CLI lingered after its result");
    }
    let stop_until = match slot.settle_by() {
        Some(settle_by) => settle_by.min(Instant::now() + STOP_BOUND),
        None => Instant::now() + STOP_BOUND,
    };
    let stopped = slot
        .bounded(
            STOP_BOUND,
            stop_by(&params.host.instance, &params.worker_session_id, stop_until),
        )
        .await
        .unwrap_or_else(|| {
            Err(CalmError::Conflict(
                "claude planner settlement stop cut off at settle_by".into(),
            ))
        });
    if let Err(error) = stopped {
        tracing::warn!(
            worker_session_id = %params.worker_session_id,
            turn_id,
            %error,
            "claude planner: settlement stop did not confirm; the next spawn's stop fails closed"
        );
    }
    let _ = child.start_kill();
    let _ = slot.bounded(REAP_WAIT, child.wait()).await;
    drop(instructions);
    let closes = translator.close_open(crate::model::now_ms());
    shared.finish_turn(bound, total_tokens);
    #[cfg(feature = "fixtures")]
    {
        let pause = shared
            .hooks
            .lock()
            .expect("hooks")
            .before_turn_completed
            .clone();
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
    }
    for notification in closes {
        let _ = shared.notifications.send(notification);
    }
    let _ = shared.notifications.send(completed);
    slot.settled.send_replace(true);
}

#[cfg(test)]
#[path = "driver_tests.rs"]
mod tests;
