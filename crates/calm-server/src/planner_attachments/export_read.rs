//! Offline evidence only. The caller must supply independently frozen root evidence;
//! metadata checks detect observed changes, they do not prove quiescence or ownership.
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub use crate::routes::fs::TrustedReadRoot;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Directory,
    Regular,
    Symlink,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Diagnostic {
    UninterpretedOwnership,
    NonUtf8Name,
    TargetNotRead,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub device: u64,
    pub inode: u64,
    pub mode: u32,
    pub size: u64,
    pub modified: (i64, i64),
    pub changed: (i64, i64),
}

#[derive(Clone, Debug)]
pub struct RawEntry {
    pub relative: PathBuf,
    pub kind: EntryKind,
    pub identity: Identity,
    pub sha256: Option<[u8; 32]>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug)]
pub struct PartialRead {
    pub relative: PathBuf,
    pub error: io::Error,
}

/// Supply each regular file (including empty files) as a read-only stream.
/// The callback must consume it fully. Retain every raw name
/// and type. Any failure invalidates completeness, including a caller sink error.
/// The root is never created; symlinks and special files are recorded, not read.
/// Unknown names, temporary files and duplicate names in different directories
/// remain distinct facts. No attachment eligibility or ownership is inferred.
/// An error discards the inventory result; callbacks may already have received
/// bytes, so callers must keep their output provisional until this returns Ok.
/// Ok means only that this traversal observed no error or identity/listing change.
/// It does not prove a frozen source, authorize publication or certify an export.
pub fn inventory_owned_tree(
    root: &TrustedReadRoot,
    mut bytes: impl FnMut(&Path, &mut dyn Read) -> io::Result<()>,
) -> Result<Vec<RawEntry>, PartialRead> {
    super::dir::inventory_raw(root, &mut bytes)
}
