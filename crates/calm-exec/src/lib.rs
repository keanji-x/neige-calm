//! calm-exec — execution contracts for issue #679 (PR1: traits only).
//!
//! Position in the target DAG (issue #679 "Crate decomposition"):
//!
//! ```text
//!   calm-server / calm-kernel / calm-provider / calm-truth
//!                        │  (all implement or drive these traits)
//!                   calm-exec   ← this crate: contracts, zero impls
//!                        │
//!                   calm-types  ← vocabulary
//! ```
//!
//! Exec sits **below** truth on purpose: truth's saga runtime drives
//! provider traits, truth's repos return `calm_types::worker::WorkerSession`,
//! and the reaction seam needs the (truth-implemented) decision sink — all
//! downward edges. Both kernel→agent and agent→truth push directions are
//! traits here, so the kernel↔provider firewall is real, not diagrammatic.
//!
//! What lives where:
//!
//! * [`provider`] — [`provider::WorkerProvider`] (issue §2: the execution-
//!   period extension of the spawn saga), the minimal [`provider::SpawnCtx`],
//!   and [`provider::SpawnHandle`] (moved verbatim from calm-server's
//!   `operation` module, which re-exports it).
//! * [`reaction`] — the agent→truth decision-write seam (issue §6: the
//!   contract FakeRoot scripts in PR5 and the real harness implements in
//!   PR6). [`reaction::DecisionIntent`] + [`reaction::AgentReactor`] +
//!   [`reaction::DecisionSink`].
//! * [`observation`] — the kernel→agent delivery seam
//!   ([`observation::ObservationSink`]; today's concrete edge is
//!   dispatcher→`HarnessRegistry`, issue §"Acyclicity hinges").
//!
//! PR1 deliberately ships **no implementations and no consumers** beyond
//! calm-server re-exporting `SpawnHandle`. FakeProvider/FakeRoot exercise
//! these traits in PR5; calm-provider implements them in PR6; the reaper
//! drives them in PR8.

pub mod observation;
pub mod provider;
pub mod reaction;

pub use observation::ObservationSink;
pub use provider::{SpawnCtx, SpawnHandle, WorkerProvider};
pub use reaction::{AgentReactor, DecisionIntent, DecisionSink};
