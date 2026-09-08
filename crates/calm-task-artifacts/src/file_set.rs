//! Explicit descriptor capture without a source directory traversal.
use crate::{
    CaptureReceipt, Entry, FileArtifactPath, FileSetCaptureRequest, OutputSlot, Result,
    SnapshotManifest, file::ordinary_source, filesystem as disk, model, store::ArtifactStore,
};
use std::fs::File;

impl ArtifactStore {
    /// Capture one nonempty explicit file set as one immutable snapshot.
    ///
    /// All stop-boundary, pinned-root, provenance and descriptor ownership rules
    /// of `capture_file` apply to every opener invocation. In particular, open
    /// read-only with O_NONBLOCK and refuse symlinks and mount crossings beneath
    /// the pinned source root. The caller must keep the entire source stopped
    /// throughout capture; the library cannot prove these assertions.
    ///
    /// Paths are opened once each in canonical order, under the store lock; the
    /// opener must not reenter this store. Frozen replay/conflict never opens a
    /// source. Invalid lists fail before opening anything. A failure before freeze
    /// publishes no snapshot or key binding; retry may open the source again.
    pub fn capture_files(
        &self,
        request: FileSetCaptureRequest<'_>,
        mut open_source: impl FnMut(&FileArtifactPath) -> Result<File>,
    ) -> Result<CaptureReceipt> {
        model::capture_identity(request.key, request.boundary_id)?;
        // Bound the caller's slice before cloning or sorting it.
        if request.paths.len() > self.limits.max_entries {
            return Err(crate::Error::Limit("entry count".into()));
        }
        model::output_name(request.output)?;
        for path in request.paths {
            model::file_path(path.as_str(), &self.limits)?;
        }
        let outputs = model::normalize_outputs(
            &[OutputSlot {
                name: request.output.into(),
                paths: request.paths.iter().map(|p| p.as_str().into()).collect(),
            }],
            &self.limits,
        )?;
        let shape = model::file_set_shape(&outputs[0].paths, &self.limits)?;
        let mut paths: Vec<_> = request.paths.iter().collect();
        paths.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let request_bytes =
            serde_json::to_vec(&("file-set-capture-v1", request.boundary_id, &outputs))?;
        self.capture_transaction(
            request.key,
            &request_bytes,
            |stage| {
                disk::private_dir(&stage.join("objects"))?;
                let mut entries: Vec<_> = shape
                    .into_iter()
                    .filter_map(|(path, directory)| directory.then_some(Entry::Directory { path }))
                    .collect();
                let mut total_bytes = 0u64;
                for path in paths {
                    let file = ordinary_source(open_source(path)?)?;
                    let entry = self.capture_object(
                        file,
                        path.as_str().into(),
                        stage,
                        self.limits
                            .max_file_bytes
                            .min(self.limits.max_total_bytes - total_bytes),
                    )?;
                    if let Entry::File { bytes, .. } = &entry {
                        total_bytes += bytes;
                    }
                    entries.push(entry);
                }
                entries.sort_by(|a, b| a.path().cmp(b.path()));
                self.write_manifest(
                    SnapshotManifest {
                        version: "file-manifest-v1".into(),
                        delivery_version: "regular-file-set-v1".into(),
                        entries,
                        outputs,
                    },
                    stage,
                )
            },
            |_| Ok(()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::OpenOptionsExt};

    #[test]
    fn file_set_frozen_sync_failure_replays_complete_snapshot_without_source() {
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
        let paths = ["out/a", "out/b"].map(|p| FileArtifactPath::new(p, &limits).unwrap());
        let request = || FileSetCaptureRequest {
            key: "set",
            boundary_id: "stopped",
            output: "result",
            paths: &paths,
        };
        let watched = root.join("captures");
        let result = disk::faults::with_sync(
            move |at| {
                if at == watched {
                    Err(std::io::Error::other("interrupted key fsync").into())
                } else {
                    Ok(())
                }
            },
            || {
                store.capture_files(request(), |_| {
                    Ok(fs::OpenOptions::new()
                        .read(true)
                        .custom_flags(nix::libc::O_NONBLOCK)
                        .open(&source)?)
                })
            },
        );
        assert!(matches!(result, Err(crate::Error::Io(_))));
        assert_eq!(fs::read_dir(root.join("captures")).unwrap().count(), 1);
        assert_eq!(fs::read_dir(root.join("snapshots")).unwrap().count(), 0);
        fs::remove_file(source).unwrap();
        let reopened = ArtifactStore::open_files(&root, limits).unwrap();
        let receipt = reopened
            .capture_files(request(), |_| panic!("frozen set opened source"))
            .unwrap();
        assert!(receipt.replayed);
        for path in &paths {
            assert_eq!(
                reopened
                    .read_snapshot_file(&receipt.snapshot, path)
                    .unwrap(),
                b"original"
            );
        }
    }
}
