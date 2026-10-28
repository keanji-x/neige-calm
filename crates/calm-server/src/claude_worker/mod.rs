//! A Claude task worker as headless `claude -p` turns on the Planner's shared protocol loop
//! (design task-worker-execution §3.2–§3.4). `role` is the worker's init check and argv,
//! `environment` its launch environment and Held release payload, `sink` its turn sink and the
//! durable turn port that sink records through. Nothing here admits or starts a process: the worker
//! turn owner and its admission (WA2-D) do, through these types.
#![cfg_attr(not(test), allow(dead_code, unused_imports))] // WA2-D wires the production entry.

mod environment;
mod role;
mod sink;

pub(crate) use environment::{WorkerEnvironment, WorkerLaunch, WorkerMcpToken, kernel_proxy};
pub(crate) use role::{WorkerArgs, WorkerStart};
pub use sink::WorkerEnd;
pub(crate) use sink::{
    WorkerInitRecord, WorkerSink, WorkerTurnKey, WorkerTurnLedger, WorkerTurnOutcome,
    WorkerTurnRecord,
};

#[cfg(test)]
mod environment_tests;
#[cfg(test)]
mod role_tests;
#[cfg(all(test, target_os = "linux"))]
mod sink_tests;
