//! SQLite-backed wave VCS snapshots.
//!
//! Two-phase spawn invariant (#310): dispatcher spawn first creates rows in an
//! event-less transaction, then later emits `CardAdded` through
//! `RepoEventWrite::log_pure_event`. Wave VCS commits anchor on persisted
//! events, not on raw rows, so the in-between row is invisible here just as it
//! is invisible to subscribers. Replay re-emits events through the same trait
//! methods, so commits regenerate as a side effect; there is no separate replay
//! path for wave-vcs.
//!
//! Commit hashes include the commit `created_at` timestamp. The tree hash is
//! the deterministic content anchor; replaying the same logical wave state can
//! reproduce the same tree hash without necessarily reproducing the same commit
//! hash. Fixture paths that seed events with `EventScope::System` also do not
//! generate wave-vcs commits because they are outside any wave scope.

pub const MANIFEST_SCHEMA_VERSION: i64 = 1;
pub const DEFAULT_PATCH_MAX_LINES: usize = 200;

pub type ObjectHash = String;
pub type CommitHash = String;

mod commit;
mod delta;
mod gc;
mod read;
mod runs;
mod snapshot;
mod store;
#[cfg(test)]
mod tests;
mod types;

pub use commit::{
    commit_events_in_tx, commit_events_with_author_in_tx, commit_in_tx, commit_tree,
    snapshot_transcripts_for_cards_in_wave,
};
pub use gc::{
    prune_all_waves_once, prune_wave_history_tx, spawn_unreferenced_object_sweeper,
    spawn_wave_history_pruner, sweep_unreferenced_objects_once,
};
pub use read::{
    cat_at, commit_belongs_to_wave, commit_record, diff, diff_with_patches, log,
    since_last_turn_block,
};
pub use snapshot::{backfill_existing_waves, snapshot_tree};
pub use store::{canonical_json_bytes, head, put_blob, tree_at};
pub use types::{
    CommitLog, CommitLogEntry, CommitRecord, DiffEntry, DiffStatus, FileDiff, HistoricalBlob,
    ManifestEntry, SinceLastTurnBlock, TreeManifest, TreeSnapshot,
};
