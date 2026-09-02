use calm_truth::error::Result;
use calm_truth::model::Task;
use sqlx::{Sqlite, Transaction};

#[cfg(unix)]
#[derive(Debug)]
enum PidObservation {
    Dead,
    Alive { state: char, start_time: u64 },
    ExistenceProbeFailed(Option<i32>),
    StatUnreadable(std::io::Error),
    StatUnparseable,
}

#[cfg(unix)]
impl std::fmt::Display for PidObservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dead => f.write_str("the pid was no longer observable"),
            Self::Alive { state, .. } => {
                write!(f, "the pid was observable in process state {state:?}")
            }
            Self::ExistenceProbeFailed(Some(errno)) => {
                let name = match *errno {
                    libc::EPERM => "EPERM",
                    libc::EINVAL => "EINVAL",
                    libc::ESRCH => "ESRCH",
                    _ => "UNKNOWN",
                };
                write!(
                    f,
                    "the pid-existence probe failed with errno {name} ({errno}: {}); treating the pid as live/unknown",
                    std::io::Error::from_raw_os_error(*errno)
                )
            }
            Self::ExistenceProbeFailed(None) => f.write_str(
                "the pid-existence probe failed with an unknown errno; treating the pid as live/unknown",
            ),
            Self::StatUnreadable(error) => {
                write!(
                    f,
                    "the pid was observable, but its /proc stat was unreadable: {error}"
                )
            }
            Self::StatUnparseable => {
                f.write_str("the pid was observable, but its /proc stat was unparseable")
            }
        }
    }
}

#[cfg(unix)]
type SignalResult = std::result::Result<(), Option<i32>>;

#[cfg(all(test, unix))]
std::thread_local! {
    static TEST_SIGNAL_INTERCEPT: std::cell::Cell<Option<(usize, SignalResult)>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(unix)]
fn send_signal(pid: i32, signal: i32) -> SignalResult {
    #[cfg(test)]
    if let Some(result) = TEST_SIGNAL_INTERCEPT.with(|intercept| {
        let (calls, result) = intercept.get()?;
        intercept.set(Some((calls + 1, result)));
        Some(result)
    }) {
        return result;
    }

    // SAFETY: `pid` and `signal` are passed straight to libc. Callers validate
    // destructive targets before requesting a nonzero signal.
    if unsafe { libc::kill(pid, signal) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().raw_os_error())
    }
}

#[cfg(unix)]
fn observe_pid_with(pid: i32, signal: impl Fn(i32, i32) -> SignalResult) -> PidObservation {
    // Signal 0 is the portable existence probe; it delivers no signal.
    if let Err(errno) = signal(pid, 0) {
        return if errno == Some(libc::ESRCH) {
            PidObservation::Dead
        } else {
            PidObservation::ExistenceProbeFailed(errno)
        };
    }

    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => match crate::proc_identity::parse_proc_stat_fields(&stat) {
            Some(fields) if fields.state == 'Z' => PidObservation::Dead,
            Some(fields) => PidObservation::Alive {
                state: fields.state,
                start_time: fields.start_time,
            },
            None => PidObservation::StatUnparseable,
        },
        Err(error) => PidObservation::StatUnreadable(error),
    }
}

#[cfg(unix)]
fn observe_pid(pid: i32) -> PidObservation {
    observe_pid_with(pid, send_signal)
}

/// Wait until `pid` disappears, or (on Linux) becomes a zombie.
///
/// The claimed-child test reads `expected_start_time` from `/proc` (on Linux)
/// after its fixture writes the pidfile and before dropping the child. When that
/// argument is `Some`, failure cleanup requires that exact pre-teardown identity
/// and does not substitute an identity observed during this poll.
///
/// The three `cli_query` callers can only read their pidfiles after the operation
/// under test has returned, so they pass `None`. On Linux, cleanup then anchors
/// to the first live `start_time` observed here. That fallback can still mistake
/// a PID recycled before the first observation for the fixture and SIGKILL it if
/// it remains alive for the full poll. We accept that pid-wraparound-shaped
/// residual because omitting cleanup would leave a 30-second fixture process on
/// every ordinary failing run. Without any readable `/proc` identity, cleanup
/// fails closed and leaves the straggler alone.
#[cfg(unix)]
pub(crate) async fn assert_pid_dead(pid: i32, expected_start_time: Option<u64>, what: &str) {
    assert!(pid > 1, "implausible descendant pid {pid}");

    let mut last_observation = None;
    let mut first_start_time = None;
    // Keep this 200 x 20 ms budget at about 4 s. The tightest claimed-child
    // fixture can spend 10 s at the spawn deadline + 4 s polling its pidfile +
    // 4 s in `assert_all_gone` + 4 s here = 22 s against its `sleep 30`.
    for _ in 0..200 {
        let observation = observe_pid(pid);
        if matches!(observation, PidObservation::Dead) {
            return;
        }
        if let PidObservation::Alive { start_time, .. } = &observation {
            first_start_time.get_or_insert(*start_time);
        }
        last_observation = Some(observation);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let last_observation = last_observation.expect("the polling loop runs at least once");
    let cleanup_start_time = expected_start_time.or(first_start_time);
    let cleanup = match cleanup_start_time {
        Some(expected) if crate::proc_identity::read_proc_start_time(pid) == Some(expected) => {
            match send_signal(pid, libc::SIGKILL) {
                Ok(()) => "identity confirmed; sent SIGKILL to the straggler".to_string(),
                Err(errno) => format!(
                    "identity confirmed, but SIGKILL failed with errno {errno:?}; the straggler may remain"
                ),
            }
        }
        Some(expected) => match crate::proc_identity::read_proc_start_time(pid) {
            Some(observed) => format!(
                "the straggler was left alone because its cleanup identity did not match: expected start_time {expected}, observed {observed}"
            ),
            None => format!(
                "the straggler was left alone because its cleanup identity could not be confirmed: expected start_time {expected}"
            ),
        },
        None => {
            "the straggler was left alone because no cleanup identity was available".to_string()
        }
    };
    panic!("{what}; last observation: {last_observation}; {cleanup}");
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use futures_util::FutureExt;

    fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
        match payload.downcast::<String>() {
            Ok(message) => *message,
            Err(payload) => match payload.downcast::<&'static str>() {
                Ok(message) => (*message).to_string(),
                Err(_) => "non-string panic payload".to_string(),
            },
        }
    }

    #[tokio::test]
    async fn assert_pid_dead_rejects_implausible_pids_before_any_signal() {
        for pid in [0, -1] {
            // If the guard is deleted, the first `kill(pid, 0)` is intercepted
            // as ESRCH: the test turns RED without delivering any real signal.
            TEST_SIGNAL_INTERCEPT.with(|intercept| {
                intercept.set(Some((0, Err(Some(libc::ESRCH)))));
            });
            let result = std::panic::AssertUnwindSafe(assert_pid_dead(
                pid,
                None,
                "invalid pid reached the polling loop",
            ))
            .catch_unwind()
            .await;
            let signal_calls = TEST_SIGNAL_INTERCEPT.with(|intercept| {
                let calls = intercept.get().map(|(calls, _)| calls);
                intercept.set(None);
                calls
            });

            assert_eq!(signal_calls, Some(0), "pid {pid} reached libc::kill");
            let message = panic_message(result.expect_err("an implausible pid must panic"));
            assert_eq!(message, format!("implausible descendant pid {pid}"));
        }
    }

    // Linux-only: this test requires a real process identity from `/proc`.
    #[cfg(target_os = "linux")]
    #[tokio::test(start_paused = true)]
    async fn assert_pid_dead_does_not_kill_a_mismatched_expected_identity() {
        let pid = std::process::id() as i32;
        let actual_start_time = crate::proc_identity::read_proc_start_time(pid)
            .expect("the test process must have a readable start_time");
        let wrong_start_time = actual_start_time ^ 1;

        // Intercept both the existence probes and any erroneous cleanup signal.
        // Exactly 200 calls means the bounded poll ran without attempting the
        // 201st call, which would be SIGKILL.
        TEST_SIGNAL_INTERCEPT.with(|intercept| intercept.set(Some((0, Ok(())))));
        let result = std::panic::AssertUnwindSafe(assert_pid_dead(
            pid,
            Some(wrong_start_time),
            "the live test process survived as intended",
        ))
        .catch_unwind()
        .await;
        let signal_calls = TEST_SIGNAL_INTERCEPT.with(|intercept| {
            let calls = intercept.get().map(|(calls, _)| calls);
            intercept.set(None);
            calls
        });

        assert_eq!(
            signal_calls,
            Some(200),
            "a mismatched expected identity reached the SIGKILL interceptor"
        );
        let message = panic_message(result.expect_err("the live pid must exhaust the poll"));
        assert!(
            message.contains("cleanup identity did not match"),
            "{message}"
        );
        assert!(
            message.contains(&format!("expected start_time {wrong_start_time}")),
            "{message}"
        );
        assert!(
            message.contains(&format!("observed {actual_start_time}")),
            "{message}"
        );
    }

    #[test]
    fn existence_probe_only_treats_esrch_as_dead() {
        let gone = observe_pid_with(42, |_, signal| {
            assert_eq!(signal, 0);
            Err(Some(libc::ESRCH))
        });
        assert!(matches!(gone, PidObservation::Dead));

        let denied = observe_pid_with(42, |_, signal| {
            assert_eq!(signal, 0);
            Err(Some(libc::EPERM))
        });
        assert!(matches!(
            denied,
            PidObservation::ExistenceProbeFailed(Some(libc::EPERM))
        ));
        let report = denied.to_string();
        assert!(report.contains("errno EPERM"), "{report}");
        assert!(report.contains("live/unknown"), "{report}");
    }
}

pub(crate) async fn insert_task_tx(tx: &mut Transaction<'_, Sqlite>, task: &Task) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO tasks
           (id,wave_id,key,kind,goal,context_json,acceptance_criteria,cwd,
            depends_on_json,priority,gate_json,status,status_detail,worker_card_id,
            gate_result_json,gate_attempt,gate_pid,gate_pid_starttime,gate_pid_boot_id,
            running_deadline_ms,spawn,created_at_ms,updated_at_ms,finished_at_ms)
           VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,
                  ?18,?19,?20,?21,?22,?23,?24)"#,
    )
    .bind(&task.id)
    .bind(&task.wave_id)
    .bind(&task.key)
    .bind(task.kind)
    .bind(&task.goal)
    .bind(&task.context_json)
    .bind(&task.acceptance_criteria)
    .bind(&task.cwd)
    .bind(&task.depends_on_json)
    .bind(task.priority)
    .bind(&task.gate_json)
    .bind(task.status)
    .bind(&task.status_detail)
    .bind(&task.worker_card_id)
    .bind(&task.gate_result_json)
    .bind(task.gate_attempt)
    .bind(task.gate_pid)
    .bind(task.gate_pid_starttime)
    .bind(&task.gate_pid_boot_id)
    .bind(task.running_deadline_ms)
    .bind(&task.spawn)
    .bind(task.created_at_ms)
    .bind(task.updated_at_ms)
    .bind(task.finished_at_ms)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
