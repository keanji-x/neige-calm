use crate::ids::TrackId;
use crate::model::Card;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use super::{CommitHash, ObjectHash};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeManifest {
    pub schema_version: i64,
    pub entries: BTreeMap<String, ManifestEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub blob_hash: ObjectHash,
    pub byte_len: u64,
    pub content_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeSnapshot {
    pub tree_hash: ObjectHash,
    pub manifest: TreeManifest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitRecord {
    pub hash: CommitHash,
    pub track_id: TrackId,
    pub parent_hash: Option<CommitHash>,
    pub tree_hash: ObjectHash,
    pub manifest_schema_version: i64,
    pub lifecycle: String,
    pub event_id: Option<i64>,
    pub created_at: i64,
    pub message: Option<String>,
    pub author: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffEntry {
    pub path: String,
    pub status: DiffStatus,
    pub old_hash: Option<ObjectHash>,
    pub new_hash: Option<ObjectHash>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffStatus {
    Added,
    Deleted,
    Modified,
}

impl DiffStatus {
    /// Every variant: `index_in_all` matches exhaustively and the const assertion below pins each
    /// variant's slot, so a new variant fails to compile until it is listed here.
    pub const ALL: [Self; 3] = [Self::Added, Self::Deleted, Self::Modified];

    const fn index_in_all(self) -> usize {
        match self {
            Self::Added => 0,
            Self::Deleted => 1,
            Self::Modified => 2,
        }
    }

    pub fn wire_label(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Deleted => "deleted",
            Self::Modified => "modified",
        }
    }

    /// Inverse of [`Self::wire_label`]; any other label is not a diff status.
    pub fn from_wire_label(label: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|status| status.wire_label() == label)
    }

    /// Human-facing label shared by the since-last-turn block and `neige diff`.
    pub fn observation_label(self) -> &'static str {
        match self {
            Self::Added => "new",
            Self::Deleted => "deleted",
            Self::Modified => "edited",
        }
    }
}

const _: () = {
    let mut index = 0;
    while index < DiffStatus::ALL.len() {
        assert!(DiffStatus::ALL[index].index_in_all() == index);
        index += 1;
    }
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub status: DiffStatus,
    pub old_hash: Option<ObjectHash>,
    pub new_hash: Option<ObjectHash>,
    pub old_content_type: Option<String>,
    pub new_content_type: Option<String>,
    pub patch: Option<String>,
    pub patch_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoricalBlob {
    pub commit: CommitHash,
    pub path: String,
    pub content: String,
    pub content_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitLogEntry {
    pub hash: CommitHash,
    pub parent_hash: Option<CommitHash>,
    pub lifecycle: String,
    pub event_id: Option<i64>,
    pub created_at: i64,
    pub message: Option<String>,
    pub changed_paths: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitLog {
    pub commits: Vec<CommitLogEntry>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SinceLastTurnBlock {
    pub current_head: Option<CommitHash>,
    pub block: Option<String>,
}

/// Whether `since_last_turn_block` inlines the `report.md` unified patch under its `report.md` line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportPatch {
    Include,
    Omit,
}

impl SinceLastTurnBlock {
    pub fn empty() -> Self {
        Self::default()
    }
}

#[derive(Clone, Debug)]
pub(super) struct BlobContent {
    pub(super) bytes: Vec<u8>,
    pub(super) content_type: String,
}

#[derive(Clone, Debug)]
pub(super) struct CardProjection {
    pub(super) card: Card,
    pub(super) role: String,
}

pub(super) enum CardVisibility {
    AnnouncedOrInherited(BTreeSet<String>),
    AllRows,
}

impl CardVisibility {
    pub(super) fn announced_only() -> Self {
        Self::AnnouncedOrInherited(BTreeSet::new())
    }

    pub(super) fn from_manifest(manifest: &TreeManifest) -> Self {
        Self::AnnouncedOrInherited(visible_card_ids_from_manifest(manifest))
    }

    pub(super) fn includes(&self, card_id: &str, announced: bool) -> bool {
        match self {
            Self::AnnouncedOrInherited(inherited) => announced || inherited.contains(card_id),
            Self::AllRows => true,
        }
    }
}

fn visible_card_ids_from_manifest(manifest: &TreeManifest) -> BTreeSet<String> {
    manifest
        .entries
        .keys()
        .filter_map(|path| {
            card_id_from_card_lens_path(path, ".meta.json")
                .or_else(|| card_id_from_card_lens_path(path, "meta.json"))
                .map(ToOwned::to_owned)
        })
        .collect()
}

fn card_id_from_card_lens_path<'a>(path: &'a str, leaf: &str) -> Option<&'a str> {
    path.strip_prefix("cards/")
        .and_then(|path| path.strip_suffix(leaf))
        .and_then(|path| path.strip_suffix('/'))
        .filter(|card_id| !card_id.contains('/'))
}

fn is_legacy_card_lens_path(path: &str) -> bool {
    card_id_from_card_lens_path(path, "meta.json").is_some()
        || card_id_from_card_lens_path(path, "payload.json").is_some()
}

pub(super) fn has_legacy_card_lens_paths(manifest: &TreeManifest) -> bool {
    manifest
        .entries
        .keys()
        .any(|path| is_legacy_card_lens_path(path))
}

#[cfg(test)]
mod tests {
    use super::DiffStatus;

    #[test]
    fn from_wire_label_inverts_wire_label_and_rejects_unknown() {
        for status in DiffStatus::ALL {
            assert_eq!(
                DiffStatus::from_wire_label(status.wire_label()),
                Some(status)
            );
        }
        for label in ["", "new", "edited", "renamed", "Added"] {
            assert_eq!(DiffStatus::from_wire_label(label), None, "{label:?}");
        }
    }
}
