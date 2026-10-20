//! The live turn's shared state (design #1791 §5.1 settlement): the first recorded
//! [`TerminalCause`], the CLI's stdin, and the stop deadline every settlement await is bounded by.

use std::sync::Mutex;
use std::time::Duration;

use tokio::process::ChildStdin;
use tokio::sync::watch;

use crate::error::CalmError;
use provider::claude::line_writer::{LineIoError, LineWriteError};
use provider::send_boundary::PreSendGate;

/// Why a turn is ending, recorded before the path that ends it acts; the first one recorded wins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalCause {
    /// A user or watchdog interrupt, or a harness shutdown.
    Interrupted,
    /// An init check or an undecodable line.
    Failed(String),
}

/// The live turn's shared state.
pub(crate) struct TurnSlot {
    cause: Mutex<Option<TerminalCause>>,
    pub(crate) stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    /// When the turn is stopped whatever the CLI does: armed by an interrupt (now + the stop
    /// timer) or a shutdown (now); the earliest arming wins.
    pub(crate) stop_at: watch::Sender<Option<tokio::time::Instant>>,
    /// [`super::session::SETTLE_AFTER_STOP`], or a fixtures override.
    settle_after_stop: Duration,
    pub(crate) settled: watch::Sender<bool>,
}

impl TurnSlot {
    pub(super) fn new(stdin: ChildStdin, settle_after_stop: Duration) -> Self {
        Self {
            cause: Mutex::new(None),
            stdin: tokio::sync::Mutex::new(Some(stdin)),
            stop_at: watch::Sender::new(None),
            settle_after_stop,
            settled: watch::Sender::new(false),
        }
    }

    pub(crate) fn record(&self, cause: TerminalCause) {
        let mut slot = self.cause.lock().expect("turn slot poisoned");
        if slot.is_none() {
            *slot = Some(cause);
        }
    }

    /// Arm the stop at `at` unless an earlier one is armed.
    pub(crate) fn arm_stop(&self, at: tokio::time::Instant) {
        self.stop_at.send_if_modified(|current| match current {
            Some(armed) if *armed <= at => false,
            Some(_) | None => {
                *current = Some(at);
                true
            }
        });
    }

    /// Whether a stop (interrupt timer or shutdown) is armed.
    pub(crate) fn stop_armed(&self) -> bool {
        self.stop_at.borrow().is_some()
    }

    /// The hard settlement deadline, once a stop is armed.
    pub(crate) fn settle_by(&self) -> Option<tokio::time::Instant> {
        self.stop_at.borrow().map(|at| at + self.settle_after_stop)
    }

    /// `fut` bounded by its own timeout and, once a stop is armed (even while `fut` runs), by
    /// [`Self::settle_by`]; `None` when either bound passed first.
    pub(crate) async fn bounded<F: std::future::Future>(
        &self,
        own: Duration,
        fut: F,
    ) -> Option<F::Output> {
        let mut stop_rx = self.stop_at.subscribe();
        tokio::select! {
            biased;
            out = fut => Some(out),
            _ = tokio::time::sleep(own) => None,
            _ = deadline_reached(&mut stop_rx, self.settle_after_stop) => None,
        }
    }

    pub(crate) fn cause(&self) -> Option<TerminalCause> {
        self.cause.lock().expect("turn slot poisoned").clone()
    }

    /// All user and protocol lines share the provider's actual writer mutex.
    pub(crate) async fn write_line(
        &self,
        line: &str,
        gate: &PreSendGate<'_>,
    ) -> Result<(), LineWriteError> {
        provider::claude::line_writer::write_line(&self.stdin, line, gate).await
    }

    pub(crate) async fn write_internal_line(
        &self,
        line: &str,
        kind: InternalLine,
    ) -> Result<(), LineWriteError> {
        let gate = match kind {
            InternalLine::Interrupt | InternalLine::ControlReply => {
                PreSendGate::new(|| Box::pin(async { Ok(()) }))
            }
        };
        self.write_line(line, &gate).await
    }
}

/// Internal protocol work cannot supply admission for a user invocation.
pub(crate) enum InternalLine {
    Interrupt,
    ControlReply,
}

pub(crate) fn write_error(error: LineWriteError) -> CalmError {
    match error {
        LineWriteError::BeforeWrite(message) => CalmError::Conflict(message),
        LineWriteError::CrossedWrite(LineIoError::Io(error)) => error.into(),
        LineWriteError::CrossedWrite(LineIoError::Timeout) => {
            CalmError::Conflict("claude did not take its stdin line in time".into())
        }
    }
}

/// Resolves once a stop is armed and `extra` has passed since its deadline; `extra` zero is the
/// stop itself, [`super::session::SETTLE_AFTER_STOP`] is `settle_by`.
pub(crate) async fn deadline_reached(
    rx: &mut watch::Receiver<Option<tokio::time::Instant>>,
    extra: Duration,
) {
    loop {
        let armed = *rx.borrow_and_update();
        match armed {
            Some(at) => tokio::select! {
                _ = tokio::time::sleep_until(at + extra) => return,
                changed = rx.changed() => if changed.is_err() {
                    tokio::time::sleep_until(at + extra).await;
                    return;
                },
            },
            None => {
                if rx.changed().await.is_err() {
                    std::future::pending::<()>().await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::TurnSlot;

    /// `bounded` gives up at `settle_by` when the stop is armed WHILE its future is pending, far
    /// ahead of the future's own bound (the settle_by arm, #1791 PR2b follow-up).
    #[tokio::test(start_paused = true)]
    async fn bounded_gives_up_at_settle_by_armed_while_pending() {
        let mut child = tokio::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn cat");
        let slot = TurnSlot::new(child.stdin.take().expect("stdin"), Duration::from_secs(2));
        let started = tokio::time::Instant::now();
        let arm = async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            slot.arm_stop(tokio::time::Instant::now());
        };
        let (bounded, ()) = tokio::join!(
            slot.bounded(Duration::from_secs(60), std::future::pending::<()>()),
            arm
        );
        assert_eq!(bounded, None);
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(3),
            "armed at 1 s + 2 s margin"
        );
    }
}
