//! The objective engine, phase 0: a pure core with no IO, no async and no clock.
//!
//! An objective is a standard a human approves (checks with roles, constraints and a
//! mode), a soft budget, a stop rule and a ledger of settled evaluations. The planner
//! drives it through [`engine::react`], which maps one [`engine::Event`] to the next
//! [`engine::State`] and the [`engine::Intent`]s a host should carry out.
//!
//! - [`definition`]: objective definitions and their validation;
//! - [`standard`]: the approved part of a definition, its hash and approval;
//! - [`check`]: the check kinds and how their outputs compare;
//! - [`verdict`]: whether a candidate may be kept;
//! - [`ledger`]: baseline, settled entries and the plateau counter;
//! - [`command`]: commands such as compaction and their admission;
//! - [`trigger`]: rules from what occurred to intents;
//! - [`engine`]: state, events, intents and the reaction.

pub mod check;
pub mod command;
pub mod definition;
pub mod diagnostic;
pub mod engine;
pub mod ids;
pub mod ledger;
pub mod standard;
pub mod trigger;
pub mod verdict;

pub use engine::{Event, Intent, State, react};
pub use verdict::{Verdict, VerdictKind, keepable};
