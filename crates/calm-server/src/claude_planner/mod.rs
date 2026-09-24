//! Claude Code as a Planner backend (#1791). This module holds the wire (`protocol`) and the pure
//! translation of a Claude turn into the Codex-shaped notifications the Planner harness consumes
//! (`translate`); the process, session and stop live beside them once wired.

pub mod protocol;
pub mod translate;

#[cfg(test)]
mod tests;
