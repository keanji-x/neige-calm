//! Git candidate binding (#1727 S4): the kernel commits and pins a worker attempt's worktree
//! as an immutable candidate, `candidate_id = delivery_id`.
//!
//! - [`delivery`]: the `task_git_deliveries` row (the persistent hand-off written before any
//!   Operation exists), the forge payload the delivery script runs under, and the failure-code
//!   mapping the settlement writes on the row.
//! - [`candidate`]: the `task_candidates` row, a byte copy of the operation result and the lease.
//! - [`view`]: the pure derivations the Planner read surface shows (`delivery.state`,
//!   `candidate.binding`).
//! - [`verification`]: `verification.state`, the gate's twelve-value read (slice 4, D8).
//! - [`abandonment`]: the `task_git_delivery_abandonments` row the Planner's abandon writes.
//! - [`action`]: `calm.task.delivery{retry|abandon}` — replay first, then admission, in one
//!   immediate transaction.
//! - [`refs`]: the candidate-ref prefix cleanup the Track-delete sweep runs (D9).
//! - [`staleness`]: `candidate.upstream`, how far a candidate's base is behind the Track
//!   repository's upstream as last known (#1777), read after `plan.list`'s transaction.
//!
//! Every write goes through a `begin_immediate_tx` transaction the caller owns (`action` owns
//! its own through `write_in_tx_typed`); no module here calls `pool.begin()`.
//!
//! Wired in: the report transaction (`decision_sink`) inserts the delivery row, `calm.task.complete`
//! and `scheduler::git_delivery` submit it, the scheduler settles it, `calm.plan.list` reads it,
//! `calm.task.delivery` retries or abandons it, the Track-delete sweep drops its refs.

pub(crate) mod abandonment;
pub(crate) mod action;
pub(crate) mod candidate;
pub(crate) mod delivery;
pub(crate) mod refs;
pub(crate) mod staleness;
pub(crate) mod verification;
pub(crate) mod view;

#[cfg(test)]
mod tests;
