//! One-deadline orchestration for the two asynchronous phases after spawn.
//!
//! Keeping the drain and reap timeouts here makes their shared-budget contract
//! directly testable with Tokio's virtual clock. Process-level tests only need
//! to prove that callers wire both phases into this primitive; they do not need
//! load-sensitive wall-clock thresholds to detect a reset budget.

use std::future::Future;

/// Why [`finish_within`] did not produce a reap result.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ChildFinishError<E> {
    Drain(E),
    TimedOut,
}

#[cfg(test)]
tokio::task_local! {
    /// Test-only phase boundaries. The deadline payload lets process-level
    /// tests freeze Tokio time only after the real child has been spawned and
    /// the relevant phase has been reached.
    pub(crate) static TEST_DRAIN_STARTED: TestPhaseObserver;
    pub(crate) static TEST_REAP_STARTED: TestPhaseObserver;
}

#[cfg(test)]
pub(crate) struct TestPhaseObserver {
    tx: tokio::sync::mpsc::UnboundedSender<tokio::time::Instant>,
    freeze_clock: bool,
}

#[cfg(test)]
impl TestPhaseObserver {
    pub(crate) fn new(
        tx: tokio::sync::mpsc::UnboundedSender<tokio::time::Instant>,
        freeze_clock: bool,
    ) -> Self {
        Self { tx, freeze_clock }
    }
}

#[cfg(test)]
fn observe_phase(
    observer: &'static tokio::task::LocalKey<TestPhaseObserver>,
    deadline: tokio::time::Instant,
) {
    let _ = observer.try_with(|observer| {
        if observer.tx.is_closed() {
            return;
        }
        if observer.freeze_clock {
            // Freeze synchronously with the observation. Even a heavily loaded
            // machine cannot spend the remaining budget between the event and
            // the test task receiving it.
            tokio::time::pause();
        }
        let _ = observer.tx.send(deadline);
    });
}

/// Drain both output streams, then reap the leader, under one absolute bound.
///
/// The reap future is not polled until phase one has completed successfully.
/// The same absolute `deadline` is passed to both `timeout_at` calls: time
/// spent draining is time the reap no longer owns.
pub(crate) async fn finish_within<D, E, R, T>(
    deadline: tokio::time::Instant,
    drain: D,
    reap: R,
) -> Result<T, ChildFinishError<E>>
where
    D: Future<Output = Result<(), E>>,
    R: Future<Output = T>,
{
    #[cfg(test)]
    observe_phase(&TEST_DRAIN_STARTED, deadline);

    match tokio::time::timeout_at(deadline, drain).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return Err(ChildFinishError::Drain(error)),
        Err(_elapsed) => return Err(ChildFinishError::TimedOut),
    }

    #[cfg(test)]
    observe_phase(&TEST_REAP_STARTED, deadline);

    tokio::time::timeout_at(deadline, reap)
        .await
        .map_err(|_elapsed| ChildFinishError::TimedOut)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    /// The contract is logical, not a host-performance target: drain and reap
    /// are ordered, and together consume exactly one budget. Virtual time makes
    /// the witness invariant under scheduler load.
    ///
    /// Mutation witness: give the reap a fresh relative timeout instead of the
    /// supplied absolute deadline; virtual time ends after `deadline`.
    #[tokio::test(start_paused = true)]
    async fn drain_and_reap_are_ordered_and_share_one_deadline() {
        let started = tokio::time::Instant::now();
        let budget = Duration::from_millis(300);
        let deadline = started + budget;
        let drain_finished = Rc::new(Cell::new(false));
        let observed_by_reap = Rc::clone(&drain_finished);

        let result = finish_within(
            deadline,
            async {
                tokio::time::sleep(Duration::from_millis(200)).await;
                drain_finished.set(true);
                Ok::<(), std::convert::Infallible>(())
            },
            async move {
                assert!(
                    observed_by_reap.get(),
                    "reap must not start until the drain has completed"
                );
                std::future::pending::<()>().await;
            },
        )
        .await;

        assert_eq!(result, Err(ChildFinishError::TimedOut));
        assert_eq!(
            tokio::time::Instant::now(),
            deadline,
            "reap gets the drain's remaining budget, not a fresh allowance"
        );
    }
}
