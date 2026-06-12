//! #679 PR1 — `Observation` / `HookKind` moved to `calm_types::observation`
//! (pure data, persisted in harness snapshots, spoken by calm-exec's
//! observation sink). Re-exported here so every
//! `crate::harness::observation::Observation` /
//! `crate::harness::Observation` path is unchanged.

pub use calm_types::observation::{HookKind, Observation};
