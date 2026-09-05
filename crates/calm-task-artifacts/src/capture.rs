use crate::store::ArtifactStore;
use crate::{CaptureRequest, Digest, Entry, Error, OutputSlot, Result, SnapshotManifest};
use crate::{filesystem as disk, model};
use std::{fs, os::unix::fs::MetadataExt, path::Path};

impl ArtifactStore {
    pub(crate) fn capture_tree(
        &self,
        request: &CaptureRequest<'_>,
        outputs: Vec<OutputSlot>,
        stage: &Path,
    ) -> Result<Digest> {
        let root = disk::open_dir(request.source.root)?;
        let source_path = fs::canonicalize(request.source.root)?;
        if disk::overlaps(&source_path, &self.root) {
            return Err(Error::Invalid("store and source must not overlap".into()));
        }
        // A linked worktree's regular gitdir marker is supported. Only the Git
        // admission check parses that metadata; the raw walker never reads it.
        // The caller supplies repository identity. Nested Git is refused.
        let marker = disk::open_beneath(&root, ".git")?.metadata()?;
        if !marker.is_file() && !marker.is_dir() {
            return Err(Error::Unsupported("Git marker".into()));
        }
        crate::git_index::inspect(request.source.root, &self.git, &self.limits)?;
        disk::private_dir(&stage.join("objects"))?;
        let mut entries = Vec::new();
        let mut total_bytes = 0u64;
        let mut pending = vec![String::new()];
        while let Some(relative) = pending.pop() {
            let directory = if relative.is_empty() {
                root.try_clone()?
            } else {
                disk::open_beneath(&root, &relative)?
            };
            if !directory.metadata()?.is_dir() {
                return Err(Error::Integrity("source directory changed".into()));
            }
            // Enumeration is anchored to the already-open directory, not a
            // re-resolved worker-controlled parent path.
            for item in fs::read_dir(disk::fd_path(&directory))? {
                let name = item?
                    .file_name()
                    .into_string()
                    .map_err(|_| Error::Unsupported("non-UTF-8 filename".into()))?;
                if relative.is_empty() && name == ".git" {
                    continue;
                }
                let name = if relative.is_empty() {
                    name
                } else {
                    format!("{relative}/{name}")
                };
                model::path(&name, &self.limits)?;
                if entries.len() >= self.limits.max_entries {
                    return Err(Error::Limit("entry count".into()));
                }
                let file = disk::open_beneath(&root, &name)?;
                let meta = file.metadata()?;
                let entry = if meta.is_dir() {
                    pending.push(name.clone());
                    Entry::Directory { path: name }
                } else {
                    disk::regular(&file)?;
                    // An external hardlink can preserve an unconfined writer;
                    // do not represent it as a supported isolated source.
                    if meta.nlink() != 1 {
                        return Err(Error::Unsupported("hard-linked source file".into()));
                    }
                    let mut object = tempfile::NamedTempFile::new_in(stage.join("objects"))?;
                    let limit = self
                        .limits
                        .max_file_bytes
                        .min(self.limits.max_total_bytes - total_bytes);
                    let (digest, bytes) = disk::copy_hash(file, &mut object, limit)?;
                    total_bytes += bytes;
                    object.as_file().sync_all()?;
                    let object_path = stage.join("objects").join(digest.as_str());
                    match object.persist_noclobber(&object_path) {
                        Ok(_) => {}
                        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
                            let existing = disk::open_beneath(
                                &disk::open_dir(&stage.join("objects"))?,
                                digest.as_str(),
                            )?;
                            let actual = disk::copy_hash(existing, &mut std::io::sink(), limit)?;
                            disk::verify_identity(&actual, &digest, bytes)?;
                        }
                        Err(e) => return Err(e.error.into()),
                    }
                    Entry::File {
                        path: name,
                        digest,
                        bytes,
                        executable: meta.mode() & 0o111 != 0,
                    }
                };
                entries.push(entry);
            }
        }
        entries.sort_by(|a, b| a.path().cmp(b.path()));
        let manifest = SnapshotManifest {
            version: "file-manifest-v1".into(),
            delivery_version: "git-v1".into(),
            entries,
            outputs,
        };
        manifest.validate(&self.limits)?;
        let bytes = serde_json::to_vec(&manifest)?;
        if bytes.len() as u64 > self.limits.max_manifest_bytes {
            return Err(Error::Limit("manifest bytes".into()));
        }
        let identity = Digest::of(&bytes);
        disk::write_new(&stage.join("manifest.json"), &bytes)?;
        disk::sync_dir(&stage.join("objects"))?;
        disk::sync_dir(stage)?;
        Ok(identity)
    }
}
