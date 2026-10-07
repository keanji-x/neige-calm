//! When a running Codex or Claude worker counts as dead (#2406). Progress is the worker card's
//! transcript capture advancing (`worker_flow_cursors.updated_at_ms`): a new message, tool call
//! or tool result. A worker that makes none for the idle window, or runs past the hard cap
//! stamped when its task entered `running`, fails as `worker-timeout`.

use std::time::Duration;

use calm_truth::db::sqlite::RunningLivenessFacts;

/// The running-worker windows, from typed configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkerLiveness {
    /// Longest stretch without transcript progress. One long command records nothing until it
    /// returns, so this covers the longest single command a worker runs.
    pub idle: Duration,
    /// Longest a worker may run, progress or not.
    pub cap: Duration,
}

impl WorkerLiveness {
    /// Floor for either window: below a minute, ordinary model latency reads as a dead worker.
    pub const MIN_SECS: u64 = 60;
    pub const DEFAULT_IDLE_SECS: u64 = 3600;
    pub const DEFAULT_CAP_SECS: u64 = 8 * 3600;
    pub const DEFAULT: Self = Self {
        idle: Duration::from_secs(Self::DEFAULT_IDLE_SECS),
        cap: Duration::from_secs(Self::DEFAULT_CAP_SECS),
    };

    pub(crate) fn cap_ms(&self) -> i64 {
        duration_ms(self.cap)
    }

    /// `floor_ms` is when this kernel booted: capture reattaches after boot and catches up
    /// asynchronously, so a cursor is not stale evidence until a full idle window after it.
    pub(crate) fn expiry(
        &self,
        facts: &RunningLivenessFacts,
        now_ms: i64,
        floor_ms: i64,
    ) -> Option<LivenessExpiry> {
        if now_ms > facts.deadline_ms {
            return Some(LivenessExpiry::Cap);
        }
        let anchor = facts
            .started_at_ms
            .max(facts.last_progress_ms.unwrap_or(i64::MIN))
            .max(floor_ms);
        (now_ms > anchor.saturating_add(duration_ms(self.idle)))
            .then_some(LivenessExpiry::Idle(self.idle))
    }
}

/// Which window a running worker outlived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LivenessExpiry {
    Idle(Duration),
    /// The row's own deadline passed. It was stamped under the cap configured when the task
    /// entered `running` (or under the earlier fixed window, for a row already running then), so
    /// the reason names no length.
    Cap,
}

impl LivenessExpiry {
    /// The `task.failed` reason the Planner reads.
    pub(crate) fn reason(self) -> String {
        match self {
            Self::Idle(idle) => format!(
                "worker made no transcript progress for {}",
                human_duration(idle)
            ),
            Self::Cap => "worker ran past its running cap".to_string(),
        }
    }
}

fn duration_ms(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn human_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs >= 3600 && secs.is_multiple_of(3600) {
        format!("{} h", secs / 3600)
    } else if secs >= 60 && secs.is_multiple_of(60) {
        format!("{} min", secs / 60)
    } else {
        format!("{} ms", duration.as_millis())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    fn liveness() -> WorkerLiveness {
        WorkerLiveness {
            idle: Duration::from_secs(60 * 60),
            cap: Duration::from_secs(8 * 60 * 60),
        }
    }

    fn facts(last_progress_ms: Option<i64>) -> RunningLivenessFacts {
        RunningLivenessFacts {
            started_at_ms: 0,
            deadline_ms: 480 * MIN,
            last_progress_ms,
        }
    }

    #[test]
    fn idle_window_runs_from_the_latest_of_start_progress_and_boot() {
        let l = liveness();
        let idle = Some(LivenessExpiry::Idle(l.idle));
        // No progress yet: the window runs from the start.
        assert_eq!(l.expiry(&facts(None), 60 * MIN, 0), None);
        assert_eq!(l.expiry(&facts(None), 60 * MIN + 1, 0), idle);
        // Progress past the old two-hour deadline keeps the worker alive.
        assert_eq!(l.expiry(&facts(Some(150 * MIN)), 200 * MIN, 0), None);
        assert_eq!(l.expiry(&facts(Some(150 * MIN)), 210 * MIN + 1, 0), idle);
        // After a reboot the cursor is not stale until a full window past boot.
        assert_eq!(l.expiry(&facts(Some(10 * MIN)), 100 * MIN, 50 * MIN), None);
        assert_eq!(
            l.expiry(&facts(Some(10 * MIN)), 110 * MIN + 1, 50 * MIN),
            idle
        );
    }

    #[test]
    fn the_cap_ends_a_worker_that_is_still_making_progress() {
        let l = liveness();
        assert_eq!(l.expiry(&facts(Some(480 * MIN)), 480 * MIN, 0), None);
        assert_eq!(
            l.expiry(&facts(Some(480 * MIN)), 480 * MIN + 1, 0),
            Some(LivenessExpiry::Cap)
        );
    }

    #[test]
    fn reasons_name_the_window() {
        assert_eq!(
            LivenessExpiry::Idle(Duration::from_secs(3600)).reason(),
            "worker made no transcript progress for 1 h"
        );
        assert_eq!(
            LivenessExpiry::Cap.reason(),
            "worker ran past its running cap"
        );
        assert_eq!(
            LivenessExpiry::Idle(Duration::from_secs(90 * 60)).reason(),
            "worker made no transcript progress for 90 min"
        );
        assert_eq!(
            LivenessExpiry::Idle(Duration::from_millis(1500)).reason(),
            "worker made no transcript progress for 1500 ms"
        );
    }
}
