//! Change waiting for observations: wake on renderer revisions and client
//! protocol events, never on a sleep-poll loop. Waiting is presentation; it
//! never touches a receipt or the physical action.
use super::client::Client;
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub const WAIT_MS_MAX: u64 = 20_000;
pub const SETTLE_MS_MAX: u64 = 2_000;
pub const SETTLE_MS_DEFAULT: u64 = 150;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WaitFor {
    #[default]
    Elapsed,
    Change,
}
impl WaitFor {
    fn name(self) -> &'static str {
        match self {
            Self::Elapsed => "elapsed",
            Self::Change => "change",
        }
    }
}

/// Validated waiting arguments shared by observe and action readbacks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WaitSpec {
    pub mode: WaitFor,
    pub budget_ms: u64,
    pub settle_ms: u64,
}
impl WaitSpec {
    pub fn new(wait_for: Option<WaitFor>, wait_ms: u64, settle_ms: Option<u64>) -> Result<Self> {
        let mode = wait_for.unwrap_or_default();
        ensure!(wait_ms <= WAIT_MS_MAX, "wait_ms must be 0..{WAIT_MS_MAX}");
        ensure!(
            settle_ms.is_none_or(|settle| settle <= SETTLE_MS_MAX),
            "settle_ms must be 0..{SETTLE_MS_MAX}"
        );
        ensure!(
            settle_ms.is_none() || mode == WaitFor::Change,
            "settle_ms requires wait_for=change"
        );
        Ok(Self {
            mode,
            budget_ms: wait_ms,
            settle_ms: settle_ms.unwrap_or(SETTLE_MS_DEFAULT),
        })
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.budget_ms <= WAIT_MS_MAX && self.settle_ms <= SETTLE_MS_MAX,
            "observation wait exceeds limits"
        );
        Ok(())
    }
}

/// What the wait did, reported verbatim on the observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    Changed,
    Unchanged,
    Exited,
    Elapsed,
}
pub struct WaitReport {
    pub mode: WaitFor,
    pub outcome: WaitOutcome,
    pub waited: Duration,
    pub settled: bool,
}
impl WaitReport {
    pub fn to_json(&self) -> Value {
        let outcome = match self.outcome {
            WaitOutcome::Changed => "changed",
            WaitOutcome::Unchanged => "unchanged",
            WaitOutcome::Exited => "exited",
            WaitOutcome::Elapsed => "elapsed",
        };
        json!({"mode":self.mode.name(),"outcome":outcome,
            "waited_ms":u64::try_from(self.waited.as_millis()).unwrap_or(u64::MAX),"settled":self.settled})
    }
}

/// Wait according to `spec` against `baseline` (the revision whose change the
/// caller cares about). Change mode returns once the projection revision
/// differs from the baseline and stayed quiet for `settle_ms`, or at the
/// budget, or when the process exited / the client went away.
pub async fn wait(client: &Client, spec: WaitSpec, baseline: u64) -> WaitReport {
    let started = Instant::now();
    let budget = Duration::from_millis(spec.budget_ms);
    if spec.mode == WaitFor::Elapsed {
        if spec.budget_ms > 0 {
            tokio::time::sleep(budget).await;
        }
        return WaitReport {
            mode: spec.mode,
            outcome: WaitOutcome::Elapsed,
            waited: budget,
            settled: false,
        };
    }
    let deadline = started + budget;
    let settle = Duration::from_millis(spec.settle_ms);
    let mut revisions = match client.entry.handle.model_view.lock() {
        Ok(view) => view.subscribe(),
        Err(_) => {
            return WaitReport {
                mode: spec.mode,
                outcome: WaitOutcome::Unchanged,
                waited: started.elapsed(),
                settled: false,
            };
        }
    };
    let mut events = client.changed();
    let stopped = |client: &Client| {
        client
            .screen
            .lock()
            .map(|state| !state.available || state.exited)
            .unwrap_or(true)
            || client
                .entry
                .exit
                .lock()
                .map(|exit| exit.is_some())
                .unwrap_or(true)
    };
    let mut changed = false;
    let mut settled = false;
    let mut exited = false;
    loop {
        events.borrow_and_update();
        let current = *revisions.borrow_and_update();
        if current != baseline {
            changed = true;
        }
        if stopped(client) {
            exited = true;
            break;
        }
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let quiet_until = if changed {
            (now + settle).min(deadline)
        } else {
            deadline
        };
        tokio::select! {
            result = revisions.changed() => {
                if result.is_err() {
                    break;
                }
            }
            result = events.changed() => {
                if result.is_err() {
                    break;
                }
            }
            _ = tokio::time::sleep_until(quiet_until.into()) => {
                if changed && quiet_until < deadline {
                    settled = true;
                }
                if changed || quiet_until >= deadline {
                    break;
                }
            }
        }
    }
    let outcome = if exited {
        WaitOutcome::Exited
    } else if changed {
        WaitOutcome::Changed
    } else {
        WaitOutcome::Unchanged
    };
    WaitReport {
        mode: spec.mode,
        outcome,
        waited: started.elapsed(),
        settled: settled && !exited,
    }
}
