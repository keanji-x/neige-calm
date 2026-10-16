//! Filesystem and Git workspace mechanisms. Lifecycle and authorization belong to the caller.
pub mod base;
pub mod command;
pub mod directory;
pub mod error;
pub mod execution;
pub mod git;
pub mod identity;
pub mod materialize;
mod metadata_lock;
pub mod recycle;
pub mod teardown;
pub mod worktree;

#[cfg(test)]
mod track_worktree_tests;
