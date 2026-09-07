//! Descriptor-only ordinary-file capture; transaction and byte algorithms are shared.
use crate::{
    CaptureReceipt, Entry, Error, FileArtifactPath, FileCaptureRequest, OutputSlot, Result,
    SnapshotId, SnapshotManifest, filesystem as disk, model, store::ArtifactStore,
};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use std::{fs::File, io::Seek, os::fd::AsRawFd};

impl ArtifactStore {
    /// Capture exactly one ordinary file; no Git, source pathname or directory walk.
    ///
    /// The caller must hold the declared stop boundary throughout this call, bind
    /// the path to the correct source attempt/root, and open beneath its pinned
    /// directory with symlinks and mount crossings refused. `open_source` must use
    /// O_NONBLOCK at open time (a FIFO must never block before returning here), and
    /// transfer an independently owned read-only file description with no concurrent
    /// offset users or writers. The library checks regular type, link count and
    /// descriptor flags, and rewinds before reading the complete bytes. It cannot
    /// prove the caller's path, stop or provenance assertions from a descriptor.
    ///
    /// The opener is invoked once only for an unfrozen key, under the store lock;
    /// it must not reenter this store. Frozen replay/conflict never invokes it.
    pub fn capture_file(
        &self,
        request: FileCaptureRequest<'_>,
        open_source: impl FnOnce() -> Result<File>,
    ) -> Result<CaptureReceipt> {
        model::capture_identity(request.key, request.boundary_id)?;
        let path = request.path.as_str();
        model::file_path(path, &self.limits)?;
        if path.split('/').count() > self.limits.max_entries {
            return Err(Error::Limit("entry count".into()));
        }
        model::output_name(request.output)?;
        let outputs = model::normalize_outputs(
            &[OutputSlot {
                name: request.output.into(),
                paths: vec![path.into()],
            }],
            &self.limits,
        )?;
        // Domain separation keeps the original Git request fingerprint byte-frozen.
        let request_bytes =
            serde_json::to_vec(&("file-capture-v1", request.boundary_id, &outputs))?;
        self.capture_transaction(
            request.key,
            &request_bytes,
            |stage| {
                let mut file = open_source()?;
                disk::regular(&file)?;
                let flags = OFlag::from_bits_truncate(
                    fcntl(file.as_raw_fd(), FcntlArg::F_GETFL).map_err(std::io::Error::from)?,
                );
                if !flags.contains(OFlag::O_NONBLOCK) || flags & OFlag::O_ACCMODE != OFlag::O_RDONLY
                {
                    return Err(Error::Invalid(
                        "source descriptor must be read-only and opened nonblocking".into(),
                    ));
                }
                file.rewind()?;
                disk::private_dir(&stage.join("objects"))?;
                let mut entries = Vec::new();
                for (slash, _) in path.match_indices('/') {
                    entries.push(Entry::Directory {
                        path: path[..slash].into(),
                    });
                }
                entries.push(self.capture_object(
                    file,
                    path.into(),
                    stage,
                    self.limits.max_file_bytes.min(self.limits.max_total_bytes),
                )?);
                self.write_manifest(
                    SnapshotManifest {
                        version: "file-manifest-v1".into(),
                        delivery_version: "regular-file-v1".into(),
                        entries,
                        outputs,
                    },
                    stage,
                )
            },
            |_| Ok(()),
        )
    }

    /// Return the exact file bytes from a verified sealed version. Limits apply
    /// before allocation, and the returned bytes are hashed again against the
    /// manifest. This grants no acceptance or consumption authority and performs
    /// no JSON or business validation. Callers must authorize the snapshot/path.
    pub fn read_snapshot_file(&self, id: &SnapshotId, path: &FileArtifactPath) -> Result<Vec<u8>> {
        model::file_path(path.as_str(), &self.limits)?;
        let snapshot = self.open_snapshot(id)?;
        let Some(Entry::File { digest, bytes, .. }) = snapshot
            .manifest()
            .entries
            .iter()
            .find(|entry| entry.path() == path.as_str())
        else {
            return Err(Error::Invalid("snapshot path is not a regular file".into()));
        };
        let root = disk::open_dir(&self.snapshot_path(id))?;
        let source = disk::open_beneath(&root, &format!("objects/{digest}"))?;
        let mut content = Vec::new();
        let actual = disk::copy_hash(source, &mut content, *bytes)?;
        disk::verify_identity(&actual, digest, *bytes)?;
        Ok(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::OpenOptionsExt};

    #[test]
    fn file_capture_frozen_sync_failure_replays_without_source() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let limits = crate::Limits {
            max_entries: 8,
            max_file_bytes: 1024,
            max_total_bytes: 1024,
            max_manifest_bytes: 4096,
            max_path_bytes: 128,
            max_depth: 8,
        };
        let store = ArtifactStore::open_files(&root, limits.clone()).unwrap();
        let source = temp.path().join("source");
        fs::write(&source, b"original").unwrap();
        let path = FileArtifactPath::new("result.json", &limits).unwrap();
        let request = || FileCaptureRequest {
            key: "operation",
            boundary_id: "stopped",
            output: "result",
            path: &path,
        };
        let watched = root.join("captures");
        let failed = disk::faults::with_sync(
            move |at| {
                if at == watched {
                    Err(std::io::Error::other("interrupted key fsync").into())
                } else {
                    Ok(())
                }
            },
            || {
                store.capture_file(request(), || {
                    Ok(fs::OpenOptions::new()
                        .read(true)
                        .custom_flags(nix::libc::O_NONBLOCK)
                        .open(&source)?)
                })
            },
        );
        assert!(matches!(failed, Err(Error::Io(_))));
        assert_eq!(fs::read_dir(root.join("captures")).unwrap().count(), 1);
        assert_eq!(fs::read_dir(root.join("snapshots")).unwrap().count(), 0);
        fs::remove_file(source).unwrap();
        let reopened = ArtifactStore::open_files(&root, limits).unwrap();
        let receipt = reopened
            .capture_file(request(), || panic!("frozen publication reopened source"))
            .unwrap();
        assert!(receipt.replayed);
        assert_eq!(
            reopened
                .read_snapshot_file(&receipt.snapshot, &path)
                .unwrap(),
            b"original"
        );
    }
}
