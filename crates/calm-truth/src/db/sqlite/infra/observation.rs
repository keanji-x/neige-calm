//! Caller-observed SQLite transaction phases. SQLx retains ownership of all database cleanup.
use std::ops::{Deref, DerefMut};
use std::panic::Location;
use std::time::{Duration, Instant};

use sqlx::{Sqlite, Transaction};
use tracing::Span;

const TARGET: &str = "calm_truth::db::transaction";

#[derive(Clone, Copy)]
pub(super) enum Phase {
    Acquire,
    Begin,
    Backoff,
    Active,
    Commit,
    Rollback,
    Finished,
}

/// Timing starts when the helper future is first polled; callsite attribution is captured earlier.
/// BEGIN includes worker/admission work, not only lock waiting. An interrupted outcome does not
/// prove rollback or physical lock release; SQLx can complete a cancelled COMMIT independently.
pub(super) struct Observation {
    caller: &'static Location<'static>,
    parent: Span,
    dispatch: tracing::Dispatch,
    started: Instant,
    phase_started: Instant,
    phase: Phase,
    elapsed: [Duration; 6],
    attempts: usize,
    begin_attempts: usize,
}

impl Observation {
    pub(super) fn new(caller: &'static Location<'static>) -> Self {
        // Capture execution context at first poll, including future.instrument/with_subscriber.
        let parent = Span::current();
        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        let now = Instant::now();
        Self {
            caller,
            parent,
            dispatch,
            started: now,
            phase_started: now,
            phase: Phase::Acquire,
            elapsed: [Duration::ZERO; 6],
            attempts: 0,
            begin_attempts: 0,
        }
    }

    pub(super) fn phase(&mut self, next: Phase) {
        self.record_phase();
        self.phase = next;
        self.phase_started = Instant::now();
        match next {
            Phase::Acquire => self.attempts += 1,
            Phase::Begin => self.begin_attempts += 1,
            _ => {}
        }
    }

    pub(super) fn admission_error(&mut self) {
        self.finish(match self.phase {
            Phase::Acquire => "acquire_error",
            Phase::Begin => "begin_error",
            _ => unreachable!("admission error follows acquisition or BEGIN"),
        });
    }

    fn record_phase(&mut self) {
        let slot = match self.phase {
            Phase::Acquire => 0,
            Phase::Begin => 1,
            Phase::Backoff => 2,
            Phase::Active => 3,
            Phase::Commit => 4,
            Phase::Rollback => 5,
            Phase::Finished => return,
        };
        self.elapsed[slot] += self.phase_started.elapsed();
    }

    pub(super) fn finish(&mut self, outcome: &'static str) {
        self.record_phase();
        self.phase = Phase::Finished;
        // No originating subscriber can receive this event. Do not register its callsite
        // in that empty context or redirect the transaction to a later subscriber.
        if self.dispatch.is::<tracing::subscriber::NoSubscriber>() {
            return;
        }
        // A cancelled task may drop outside its polling dispatcher. Keep observations with
        // the originating subscriber rather than silently losing or misrouting terminal data.
        tracing::dispatcher::with_default(&self.dispatch, || {
            tracing::debug!(
                target: TARGET,
                parent: &self.parent,
                source_file = self.caller.file(),
                source_line = self.caller.line(),
                outcome,
                attempts = self.attempts,
                begin_attempts = self.begin_attempts,
                acquire_ms = self.elapsed[0].as_secs_f64() * 1000.0,
                begin_ms = self.elapsed[1].as_secs_f64() * 1000.0,
                backoff_ms = self.elapsed[2].as_secs_f64() * 1000.0,
                active_ms = self.elapsed[3].as_secs_f64() * 1000.0,
                commit_ms = self.elapsed[4].as_secs_f64() * 1000.0,
                rollback_ms = self.elapsed[5].as_secs_f64() * 1000.0,
                total_ms = self.started.elapsed().as_secs_f64() * 1000.0,
                "sqlite transaction finished"
            )
        });
    }
}

impl Drop for Observation {
    fn drop(&mut self) {
        let outcome = match self.phase {
            Phase::Acquire => "acquire_interrupted",
            Phase::Begin => "begin_interrupted",
            Phase::Backoff => "backoff_interrupted",
            Phase::Active => "active_dropped",
            Phase::Commit => "commit_interrupted",
            Phase::Rollback => "rollback_interrupted",
            Phase::Finished => return,
        };
        self.finish(outcome);
    }
}

/// An owned SQLx transaction with one terminal observation. Raw `_tx` domain helpers continue
/// borrowing SQLx's transaction; SQLx alone commits, rolls back and queues rollback on Drop.
pub struct ObservedTransaction<'c> {
    // Field order matters: on active drop SQLx queues its rollback before the observation ends.
    transaction: Option<Transaction<'c, Sqlite>>,
    observation: Observation,
}

impl<'c> ObservedTransaction<'c> {
    pub(super) fn new(transaction: Transaction<'c, Sqlite>, mut observation: Observation) -> Self {
        observation.phase(Phase::Active);
        Self {
            transaction: Some(transaction),
            observation,
        }
    }

    pub async fn commit(mut self) -> std::result::Result<(), sqlx::Error> {
        self.observation.phase(Phase::Commit);
        let transaction = self.transaction.take().expect("transaction not finalized");
        let result = transaction.commit().await;
        self.observation.finish(if result.is_ok() {
            "committed"
        } else {
            "commit_error"
        });
        result
    }

    pub async fn rollback(mut self) -> std::result::Result<(), sqlx::Error> {
        self.observation.phase(Phase::Rollback);
        let transaction = self.transaction.take().expect("transaction not finalized");
        let result = transaction.rollback().await;
        self.observation.finish(if result.is_ok() {
            "rolled_back"
        } else {
            "rollback_error"
        });
        result
    }
}

impl<'c> Deref for ObservedTransaction<'c> {
    type Target = Transaction<'c, Sqlite>;

    fn deref(&self) -> &Self::Target {
        self.transaction
            .as_ref()
            .expect("transaction not finalized")
    }
}

impl DerefMut for ObservedTransaction<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.transaction
            .as_mut()
            .expect("transaction not finalized")
    }
}

#[cfg(test)]
mod tests;
