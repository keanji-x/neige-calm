//! Git candidate binding (#1727 S4): the kernel commits and pins a worker attempt's worktree
//! as an immutable candidate, `candidate_id = delivery_id`.
//!
//! - [`delivery`]: the `task_git_deliveries` row (the persistent hand-off written before any
//!   Operation exists), the forge payload the delivery script runs under, and the failure-code
//!   mapping the settlement writes on the row.
//! - [`candidate`]: the `task_candidates` row, a byte copy of the operation result and the lease.
//! - [`view`]: the pure derivations the Planner read surface shows (`delivery.state`,
//!   `candidate.binding`).
//!
//! Every write goes through a `begin_immediate_tx` transaction the caller owns; this module never
//! begins one.
//!
//! Wired in: the report transaction (`decision_sink`) inserts the delivery row, `calm.task.complete`
//! and `scheduler::git_delivery` submit it, the scheduler settles it, `calm.plan.list` reads it.

pub(crate) mod candidate;
pub(crate) mod delivery;
pub(crate) mod view;

#[cfg(test)]
mod tests;
