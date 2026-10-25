//! Shared stream-json decoding, stop/drain and execution settlement; no role state lives here.
use super::driver::TerminalEvent;
use super::role::ProtocolProfile;
use super::turn_sink::TurnSink;
use super::turn_slot::{TerminalCause, TurnSlot, deadline_reached};
use crate::provider_execution::{self as execution, stdio::EngineStdio};
use provider::claude::protocol::{Record, decode};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt as _, BufReader, Lines};
use tokio::time::Instant;
use uuid::Uuid;
use workspace_runtime::execution::*;
const DRAIN_WINDOW: Duration = Duration::from_secs(1);
const EXIT_WAIT: Duration = Duration::from_secs(5);
const DRAIN_AFTER_EXIT: Duration = Duration::from_secs(1);
/// Hard settlement margin after the stop deadline; every cleanup await is capped by it.
/// Planner's 10 s stop timer plus this margin fits the harness's 30 s interrupt budget,
/// leaving 8 s for the surrounding run loop.
pub const SETTLE_AFTER_STOP: Duration = Duration::from_secs(12);

pub(crate) struct ProtocolTurn<P> {
    pub claim: execution::Claim,
    pub guard: execution::DriverGuard,
    pub stdout: EngineStdio,
    pub profile: P,
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

pub(crate) async fn drive<S: TurnSink, P: ProtocolProfile>(
    mut sink: S,
    run: ProtocolTurn<P>,
) -> bool {
    let ProtocolTurn {
        claim,
        guard,
        stdout,
        profile,
    } = run;
    let mut cancellation = sink.cancellation(&claim);
    let engine = cancellation.engine.clone();
    let slot = sink.slot();
    let id = execution::execution_id(claim.record()).expect("admitted execution id");
    let mut ticks = tokio::time::interval(Duration::from_millis(10));
    let mut lines = BufReader::new(stdout).lines();
    let mut stop_rx = slot.stop_at.subscribe();
    let mut reading = Reading {
        sink: &mut sink,
        slot: &slot,
        profile: &profile,
        answering: true,
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
                    state: ExecutionState::Exited { .. } | ExecutionState::Drained { .. } | ExecutionState::OriginalGroupEmpty { .. } | ExecutionState::NoChild { .. } | ExecutionState::Revoked, ..
                })) { exited = Some(Instant::now()); }
            }
            _ = sleep_until_opt(drain_until), if drain_until.is_some() => {
                break Ending::Exited;
            }
        }
    };
    let qualified = settle(sink, &cancellation, &claim, ending).await;
    cancellation.armed = false;
    drop(guard);
    qualified
}

struct Reading<'a, S, P> {
    sink: &'a mut S,
    slot: &'a Arc<TurnSlot>,
    profile: &'a P,
    answering: bool,
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

impl<S: TurnSink, P: ProtocolProfile> Reading<'_, S, P> {
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
        if session
            .session_id
            .is_some_and(|id| id != self.profile.thread())
        {
            return Some(self.protocol_failure(&"native event names another session"));
        }
        if let Record::SystemInit(init) = &record {
            self.sink.observe_init(init).await;
            if let Err(check) = self.profile.init_check(init) {
                self.slot
                    .record(TerminalCause::Failed(format!("check: {check}")));
                return Some(Ending::Stopped);
            }
            self.validated_init = true;
        }
        self.sink.record(line, &record, self.answering).await;
        match record {
            Record::ResultSuccess(success) => Some(Ending::Result(self.sink.result_event(
                TerminalEvent::ResultSuccess {
                    is_error: success.is_error,
                    result: success.result,
                },
            ))),
            Record::ResultError(error) => Some(Ending::Result(self.sink.result_event(
                TerminalEvent::ResultError {
                    errors: error.errors,
                },
            ))),
            _ => None,
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
}

async fn sleep_until_opt(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Every protocol end requests Stop. Only confirmed foreground settlement qualifies a known result.
async fn settle<S: TurnSink>(
    sink: S,
    runtime: &execution::Cancellation,
    claim: &execution::Claim,
    ending: Ending,
) -> bool {
    let slot = sink.slot();
    let owner = &runtime.owner;
    let engine = &runtime.engine;
    let pool = &runtime.pool;
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
    crate::test_seams::pause_point("c4-protocol-ended", &runtime.session).await;
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
                                | ExecutionState::OriginalGroupEmpty { .. }
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
            pool,
            Some(engine),
            &runtime.session,
            claim,
            slot.stop_reason(),
        )
        .await
    {
        tracing::warn!(%error, "Claude Stop retained claim");
    }
    let _ = slot
        .bounded(
            SETTLE_AFTER_STOP,
            execution::wait_settled(engine, &id, SETTLE_AFTER_STOP),
        )
        .await;
    let settlement = match execution::settlement(pool, &id).await {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "Claude settlement read failed; completion is unconfirmed");
            None
        }
    };
    let qualified = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let known = sink.known_end(claim, event, cause, qualified.clone());
    #[cfg(feature = "fixtures")]
    let pass = crate::test_seams::provider_pass::driver(
        owner,
        &runtime.session,
        settlement.is_some(),
        &known,
    )
    .await;
    let result = if settlement.is_some() {
        owner
            .finish(pool, Some(engine), &runtime.session, claim, known)
            .await
    } else {
        owner
            .owner
            .complete(
                &runtime.session,
                claim.record(),
                execution::CompletionEvidence::Unconfirmed,
                known,
            )
            .await
    };
    if let Err(error) = result {
        tracing::warn!(%error, "Claude completion retained claim");
    }
    #[cfg(feature = "fixtures")]
    pass.returned();
    qualified.load(std::sync::atomic::Ordering::Acquire)
}
