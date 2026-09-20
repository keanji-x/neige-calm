//! SQLite-backed track VCS snapshots. Commits anchor on persisted events, not
//! raw rows; the tree hash is the deterministic content anchor, while commit
//! hashes include `created_at` and need not reproduce on replay.

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
    snapshot_transcripts_for_cards_in_track,
};
pub use gc::{
    prune_all_tracks_once, prune_track_history_tx, spawn_track_history_pruner,
    spawn_unreferenced_object_sweeper, sweep_unreferenced_objects_once,
};
pub use read::{
    cat_at, commit_belongs_to_track, commit_record, diff, diff_with_patches, log,
    resolve_commit_prefix, since_last_turn_block,
};
pub use snapshot::{backfill_existing_tracks, snapshot_tree};
pub use store::{canonical_json_bytes, head, put_blob, tree_at};
pub use types::{
    CommitLog, CommitLogEntry, CommitRecord, DiffEntry, DiffStatus, FileDiff, HistoricalBlob,
    ManifestEntry, ReportPatch, SinceLastTurnBlock, TreeManifest, TreeSnapshot,
};
