use crate::{
    CaptureReceipt, CaptureRequest, Digest, Entry, Error, GitConfig, Limits, Result, Snapshot,
    SnapshotId, SnapshotManifest,
};
use crate::{filesystem as disk, model};
use nix::fcntl::{Flock, FlockArg};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

const FORMAT: &[u8] = b"calm-task-artifacts/file-manifest-v1\n";

/// Synchronous filesystem library; async callers should use their blocking pool.
/// Available on Linux with openat2/renameat2 and a local fsync-capable filesystem.
#[derive(Debug)]
pub struct ArtifactStore {
    pub(crate) root: PathBuf,
    pub(crate) limits: Limits,
    pub(crate) git: GitConfig,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureRecord {
    version: String,
    key_digest: Digest,
    fingerprint: Digest,
    snapshot: SnapshotId,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CapturePoint {
    Staged,
    Frozen,
    Published,
}

impl ArtifactStore {
    /// Open/create a private store. The caller must keep its parent stable and
    /// exclude this tree from every worker's writable mounts. Only abandoned,
    /// unpublished capture staging is removed, under an exclusive process lock.
    pub fn open(root: &Path, limits: Limits, git: GitConfig) -> Result<Self> {
        limits.validate()?;
        disk::absolute(&git.binary)?;
        if git.timeout.is_zero() {
            return Err(Error::Invalid(
                "Git inspection timeout must be positive".into(),
            ));
        }
        disk::absolute(root)?;
        disk::open_dir(
            root.parent()
                .ok_or_else(|| Error::Invalid("store parent".into()))?,
        )?;
        disk::private_dir(root)?;
        let store = Self {
            root: fs::canonicalize(root)?,
            limits,
            git,
        };
        // Refuse unrelated data before creating even our lock file there.
        if fs::symlink_metadata(store.root.join("FORMAT")).is_err() {
            for entry in fs::read_dir(&store.root)? {
                let name = entry?.file_name();
                if name != ".lock" && name != "FORMAT" {
                    return Err(Error::Invalid(
                        "refusing to adopt a nonempty directory".into(),
                    ));
                }
            }
        }
        let _lock = store.lock()?;
        let root_handle = disk::open_dir(&store.root)?;
        match disk::open_beneath(&root_handle, "FORMAT") {
            Ok(file) => {
                if disk::read_bounded(file, FORMAT.len() as u64)? != FORMAT {
                    return Err(Error::Unsupported("store format".into()));
                }
            }
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                for entry in fs::read_dir(&store.root)? {
                    if entry?.file_name() != ".lock" {
                        return Err(Error::Invalid(
                            "refusing to adopt a nonempty directory".into(),
                        ));
                    }
                }
                disk::write_new(&store.root.join("FORMAT"), FORMAT)?;
                disk::sync_dir(&store.root)?;
            }
            Err(e) => return Err(e),
        }
        for name in ["staging", "captures", "snapshots"] {
            disk::private_dir(&store.root.join(name))?;
        }
        disk::clean_staging(&store.root.join("staging"))?;
        Ok(store)
    }

    pub(crate) fn lock(&self) -> Result<Flock<File>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
            .open(self.root.join(".lock"))?;
        disk::regular(&file)?;
        Flock::lock(file, FlockArg::LockExclusive).map_err(|(_, e)| std::io::Error::from(e).into())
    }

    /// Freeze raw candidate bytes and declared slots. Once the request is frozen,
    /// repeating its key/spec returns that original snapshot without re-reading
    /// the source, even after source deletion. A changed boundary/spec conflicts.
    /// A failure after durable freeze is recoverable by retrying the same request.
    pub fn capture(&self, request: CaptureRequest<'_>) -> Result<CaptureReceipt> {
        self.capture_inner(request, |_| Ok(()))
    }

    pub(crate) fn capture_inner(
        &self,
        request: CaptureRequest<'_>,
        mut checkpoint: impl FnMut(CapturePoint) -> Result<()>,
    ) -> Result<CaptureReceipt> {
        for value in [request.key, request.source.boundary_id] {
            if value.is_empty() || value.len() > 512 || value.bytes().any(|b| b.is_ascii_control())
            {
                return Err(Error::Invalid(
                    "capture key and trusted boundary ID must be nonempty bounded strings".into(),
                ));
            }
        }
        let outputs = model::normalize_outputs(request.outputs, &self.limits)?;
        let request_bytes =
            serde_json::to_vec(&("capture-v1", request.source.boundary_id, &outputs))?;
        if request_bytes.len() as u64 > self.limits.max_manifest_bytes {
            return Err(Error::Limit("request bytes".into()));
        }
        let fingerprint = Digest::of(&request_bytes);
        let key_digest = Digest::of(request.key.as_bytes());
        let request_dir = self.root.join("captures").join(key_digest.as_str());
        let _lock = self.lock()?;
        match disk::open_dir(&request_dir) {
            Ok(handle) => {
                let bytes = disk::read_bounded(
                    disk::open_beneath(&handle, "request.json")?,
                    self.limits.max_manifest_bytes,
                )?;
                let record: CaptureRecord = serde_json::from_slice(&bytes)?;
                if record.version != "capture-v1" || record.key_digest != key_digest {
                    return Err(Error::Integrity("capture record identity".into()));
                }
                if record.fingerprint != fingerprint {
                    return Err(Error::Conflict);
                }
                self.finish_capture(&request_dir, &record)?;
                return self.receipt(&record.snapshot, true);
            }
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let stage = tempfile::Builder::new()
            .prefix("capture-")
            .rand_bytes(12)
            .tempdir_in(self.root.join("staging"))?;
        let snapshot_stage = stage.path().join("snapshot");
        disk::private_dir(&snapshot_stage)?;
        let snapshot = self.capture_tree(&request, outputs, &snapshot_stage)?;
        let record = CaptureRecord {
            version: "capture-v1".into(),
            key_digest,
            fingerprint,
            snapshot,
        };
        let bytes = serde_json::to_vec(&record)?;
        if bytes.len() as u64 > self.limits.max_manifest_bytes {
            return Err(Error::Limit("capture record bytes".into()));
        }
        disk::write_new(&stage.path().join("request.json"), &bytes)?;
        disk::sync_dir(stage.path())?;
        checkpoint(CapturePoint::Staged)?;
        // This directory is the durable request intent. It precedes snapshot
        // publication, so a lost response can never select a new source version.
        disk::rename_new(stage.path(), &request_dir)?;
        disk::sync_dir(&self.root.join("captures"))?;
        disk::sync_dir(&self.root.join("staging"))?;
        checkpoint(CapturePoint::Frozen)?;
        self.finish_capture(&request_dir, &record)?;
        checkpoint(CapturePoint::Published)?;
        self.receipt(&record.snapshot, false)
    }

    fn finish_capture(&self, request_dir: &Path, record: &CaptureRecord) -> Result<()> {
        let staged = request_dir.join("snapshot");
        let published = self.snapshot_path(&record.snapshot);
        match disk::open_dir(&published) {
            Ok(_) => {
                self.load_snapshot(&published, &record.snapshot)?;
                match disk::open_dir(&staged) {
                    Ok(_) => {
                        self.load_snapshot(&staged, &record.snapshot)?;
                        fs::remove_dir_all(&staged)?;
                    }
                    Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                self.load_snapshot(&staged, &record.snapshot)?;
                disk::rename_new(&staged, &published)?;
            }
            Err(e) => return Err(e),
        }
        disk::sync_dir(&self.root.join("snapshots"))?;
        disk::sync_dir(request_dir)?;
        Ok(())
    }
    fn receipt(&self, id: &SnapshotId, replayed: bool) -> Result<CaptureReceipt> {
        let snapshot = self.open_snapshot(id)?;
        Ok(CaptureReceipt {
            snapshot: id.clone(),
            replayed,
            missing_outputs: snapshot.missing_outputs(),
        })
    }
    pub(crate) fn snapshot_path(&self, id: &SnapshotId) -> PathBuf {
        self.root.join("snapshots").join(id.as_str())
    }

    /// Validate the canonical manifest AND every file object before returning it.
    /// Snapshot identity is the hash of manifest bytes; no mutable handle is exposed.
    pub fn open_snapshot(&self, id: &SnapshotId) -> Result<Snapshot> {
        self.load_snapshot(&self.snapshot_path(id), id)
    }
    pub(crate) fn load_snapshot(&self, location: &Path, id: &SnapshotId) -> Result<Snapshot> {
        let root = disk::open_dir(location)?;
        let bytes = disk::read_bounded(
            disk::open_beneath(&root, "manifest.json")?,
            self.limits.max_manifest_bytes,
        )?;
        if Digest::of(&bytes) != *id {
            return Err(Error::Integrity("snapshot manifest digest".into()));
        }
        let manifest: SnapshotManifest = serde_json::from_slice(&bytes)?;
        manifest.validate(&self.limits)?;
        if serde_json::to_vec(&manifest)? != bytes {
            return Err(Error::Integrity("noncanonical manifest encoding".into()));
        }
        for entry in &manifest.entries {
            if let Entry::File { digest, bytes, .. } = entry {
                let object = disk::open_beneath(&root, &format!("objects/{digest}"))?;
                let actual = disk::copy_hash(object, &mut std::io::sink(), *bytes)?;
                disk::verify_identity(&actual, digest, *bytes)?;
            }
        }
        Ok(Snapshot {
            id: id.clone(),
            manifest,
        })
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
