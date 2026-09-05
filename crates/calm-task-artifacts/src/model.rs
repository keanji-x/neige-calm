use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
};

/// Checked SHA-256 identity. Deserialization never accepts a path-like value.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest(String);

impl Digest {
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub(crate) fn of(bytes: &[u8]) -> Self {
        Self(hex::encode(Sha256::digest(bytes)))
    }
    pub(crate) fn from_hash(hash: Sha256) -> Self {
        Self(hex::encode(hash.finalize()))
    }
}
impl TryFrom<String> for Digest {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Invalid("expected lowercase SHA-256 identity".into()));
        }
        Ok(Self(value))
    }
}
impl From<Digest> for String {
    fn from(value: Digest) -> Self {
        value.0
    }
}
impl FromStr for Digest {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        value.to_owned().try_into()
    }
}
impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
pub type SnapshotId = Digest;

/// Trusted system Git binary and bounded read-only index inspection.
#[derive(Clone, Debug)]
pub struct GitConfig {
    pub binary: PathBuf,
    pub timeout: std::time::Duration,
}

/// Required resource ceilings, also enforced when reopening and preparing data.
#[derive(Clone, Debug)]
pub struct Limits {
    pub max_entries: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub max_manifest_bytes: u64,
    pub max_path_bytes: usize,
    pub max_depth: usize,
}
impl Limits {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.max_entries == 0
            || self.max_file_bytes == 0
            || self.max_total_bytes == 0
            || self.max_manifest_bytes == 0
            || self.max_manifest_bytes == u64::MAX
            || self.max_path_bytes == 0
            || self.max_depth == 0
        {
            return Err(Error::Invalid(
                "resource limits must be positive and bounded".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSlot {
    pub name: String,
    pub paths: Vec<String>,
}

/// The caller asserts this is an isolated local Git worktree whose writers have
/// stopped. No filesystem check in this crate establishes process quiescence.
pub struct QuiescentSource<'a> {
    pub root: &'a Path,
    pub boundary_id: &'a str,
}
pub struct CaptureRequest<'a> {
    pub key: &'a str,
    pub source: QuiescentSource<'a>,
    pub outputs: &'a [OutputSlot],
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MissingOutput {
    pub output: String,
    pub paths: Vec<String>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureReceipt {
    pub snapshot: SnapshotId,
    pub replayed: bool,
    pub missing_outputs: Vec<MissingOutput>,
}

/// Only directory identity and a file's bytes/executable bit survive capture.
/// UID, timestamps, xattrs and setuid/setgid bits are deliberately not copied.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Entry {
    Directory {
        path: String,
    },
    File {
        path: String,
        digest: Digest,
        bytes: u64,
        executable: bool,
    },
}
impl Entry {
    pub fn path(&self) -> &str {
        match self {
            Self::Directory { path } | Self::File { path, .. } => path,
        }
    }
    pub(crate) fn at(&self, new_path: String) -> Self {
        let mut entry = self.clone();
        match &mut entry {
            Self::Directory { path } | Self::File { path, .. } => *path = new_path,
        }
        entry
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotManifest {
    pub version: String,
    pub delivery_version: String,
    pub entries: Vec<Entry>,
    pub outputs: Vec<OutputSlot>,
}
impl SnapshotManifest {
    pub(crate) fn validate(&self, limits: &Limits) -> Result<()> {
        if self.version != "file-manifest-v1" || self.delivery_version != "git-v1" {
            return Err(Error::Unsupported("manifest version".into()));
        }
        validate_entries(&self.entries, limits)?;
        if normalize_outputs(&self.outputs, limits)? != self.outputs {
            return Err(Error::Integrity("noncanonical output declarations".into()));
        }
        Ok(())
    }
    pub(crate) fn missing_outputs(&self) -> Vec<MissingOutput> {
        self.outputs
            .iter()
            .filter_map(|slot| {
                let paths: Vec<_> = slot
                    .paths
                    .iter()
                    .filter(|p| !self.entries.iter().any(|e| e.path() == *p))
                    .cloned()
                    .collect();
                (!paths.is_empty()).then(|| MissingOutput {
                    output: slot.name.clone(),
                    paths,
                })
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub(crate) id: SnapshotId,
    pub(crate) manifest: SnapshotManifest,
}
impl Snapshot {
    pub fn id(&self) -> &SnapshotId {
        &self.id
    }
    pub fn manifest(&self) -> &SnapshotManifest {
        &self.manifest
    }
    pub fn missing_outputs(&self) -> Vec<MissingOutput> {
        self.manifest.missing_outputs()
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotBinding {
    pub snapshot: SnapshotId,
    pub output: String,
    pub into: String,
}
#[derive(Clone, Debug)]
pub struct Materialized {
    pub destination: PathBuf,
    pub entries: Vec<Entry>,
}

pub(crate) fn path(value: &str, limits: &Limits) -> Result<()> {
    if value.is_empty()
        || value.starts_with('/')
        || value.contains('\\')
        || value.bytes().any(|b| b.is_ascii_control())
        || value
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == ".." || p == ".git" || p == ".gitmodules")
    {
        return Err(Error::Invalid(format!(
            "unsupported relative artifact path: {value:?}"
        )));
    }
    if value.len() > limits.max_path_bytes || value.split('/').count() > limits.max_depth {
        return Err(Error::Limit("path length or depth".into()));
    }
    Ok(())
}
pub(crate) fn contains(parent: &str, child: &str) -> bool {
    child == parent
        || child
            .strip_prefix(parent)
            .is_some_and(|tail| tail.starts_with('/'))
}
pub(crate) fn disjoint(paths: &[&str]) -> Result<()> {
    let mut sorted = paths.to_vec();
    sorted.sort_unstable();
    // Check all ancestors, not just adjacent lexicographic entries (`a-` sorts
    // between `a` and `a/b`). Every path has already been depth-bounded.
    for (index, candidate) in sorted.iter().enumerate() {
        if index > 0 && sorted[index - 1] == *candidate {
            return Err(Error::Invalid("duplicate artifact path".into()));
        }
        for (slash, _) in candidate.match_indices('/') {
            if sorted.binary_search(&&candidate[..slash]).is_ok() {
                return Err(Error::Invalid("overlapping artifact paths".into()));
            }
        }
    }
    Ok(())
}
pub(crate) fn normalize_outputs(
    outputs: &[OutputSlot],
    limits: &Limits,
) -> Result<Vec<OutputSlot>> {
    if outputs.len() > limits.max_entries {
        return Err(Error::Limit("output slots".into()));
    }
    let mut total_paths = 0usize;
    for slot in outputs {
        if slot.name.is_empty()
            || slot.name.len() > 128
            || !slot
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || slot.paths.is_empty()
        {
            return Err(Error::Invalid(
                "slot needs a name and at least one path".into(),
            ));
        }
        total_paths = total_paths
            .checked_add(slot.paths.len())
            .ok_or_else(|| Error::Limit("output paths".into()))?;
        if total_paths > limits.max_entries {
            return Err(Error::Limit("output paths".into()));
        }
        for value in &slot.paths {
            path(value, limits)?;
        }
    }
    disjoint(
        &outputs
            .iter()
            .flat_map(|s| s.paths.iter().map(String::as_str))
            .collect::<Vec<_>>(),
    )?;
    let mut normalized = outputs.to_vec();
    for slot in &mut normalized {
        slot.paths.sort();
    }
    normalized.sort_by(|a, b| a.name.cmp(&b.name));
    if normalized.windows(2).any(|w| w[0].name == w[1].name) {
        return Err(Error::Invalid("duplicate output name".into()));
    }
    Ok(normalized)
}
pub(crate) fn validate_entries(entries: &[Entry], limits: &Limits) -> Result<()> {
    if entries.len() > limits.max_entries {
        return Err(Error::Limit("entry count".into()));
    }
    let mut seen = BTreeMap::new();
    let mut previous = None;
    let mut total = 0u64;
    for entry in entries {
        let name = entry.path();
        path(name, limits)?;
        if previous.is_some_and(|p| p >= name) {
            return Err(Error::Integrity("noncanonical candidate paths".into()));
        }
        previous = Some(name);
        if let Some((parent, _)) = name.rsplit_once('/')
            && seen.get(parent) != Some(&true)
        {
            return Err(Error::Integrity("missing directory parent".into()));
        }
        seen.insert(name, matches!(entry, Entry::Directory { .. }));
        if let Entry::File { bytes, .. } = entry {
            total = total
                .checked_add(*bytes)
                .ok_or_else(|| Error::Limit("total bytes".into()))?;
            if *bytes > limits.max_file_bytes || total > limits.max_total_bytes {
                return Err(Error::Limit("file or total bytes".into()));
            }
        }
    }
    Ok(())
}
