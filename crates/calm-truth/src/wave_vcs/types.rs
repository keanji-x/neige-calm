use crate::ids::WaveId;
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
    pub wave_id: WaveId,
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
    pub fn wire_label(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Deleted => "deleted",
            Self::Modified => "modified",
        }
    }

    pub(super) fn observation_label(self) -> &'static str {
        match self {
            Self::Added => "new",
            Self::Deleted => "deleted",
            Self::Modified => "edited",
        }
    }
}

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
