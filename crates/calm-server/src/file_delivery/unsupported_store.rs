//! Server-side refusal adapter; the artifact library remains Linux-only.
use calm_task_artifacts::{
    CaptureReceipt, Error, FileArtifactPath, FileCaptureRequest, FileSetCaptureRequest, Limits,
    Materialized, Result, SlotBinding, Snapshot, SnapshotId,
};
use std::{fs::File, path::Path};

// No successful constructor: unsupported hosts cannot acquire store authority.
pub(crate) struct ArtifactStore {
    _private: (),
}

fn unsupported<T>() -> Result<T> {
    Err(Error::Unsupported("file delivery requires Linux".into()))
}

impl ArtifactStore {
    pub(super) fn open_files(_root: &Path, _limits: Limits) -> Result<Self> {
        unsupported()
    }
    pub(super) fn capture_file(
        &self,
        _request: FileCaptureRequest<'_>,
        _open_source: impl FnOnce() -> Result<File>,
    ) -> Result<CaptureReceipt> {
        unsupported()
    }
    pub(super) fn capture_files(
        &self,
        _request: FileSetCaptureRequest<'_>,
        _open_source: impl FnMut(&FileArtifactPath) -> Result<File>,
    ) -> Result<CaptureReceipt> {
        unsupported()
    }
    pub(super) fn read_snapshot_file(
        &self,
        _id: &SnapshotId,
        _path: &FileArtifactPath,
    ) -> Result<Vec<u8>> {
        unsupported()
    }
    pub(super) fn open_snapshot(&self, _id: &SnapshotId) -> Result<Snapshot> {
        unsupported()
    }
    pub(super) fn materialize(
        &self,
        _inputs: &[SlotBinding],
        _destination: &Path,
    ) -> Result<Materialized> {
        unsupported()
    }
    pub(super) fn verify_materialized(
        &self,
        _inputs: &[SlotBinding],
        _destination: &Path,
    ) -> Result<Materialized> {
        unsupported()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_store_refuses_before_creating_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        assert!(matches!(
            ArtifactStore::open_files(&root, super::super::limits()),
            Err(Error::Unsupported(_))
        ));
        assert!(!root.exists());
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("sentinel"), "unchanged").unwrap();
        assert!(matches!(
            ArtifactStore::open_files(&root, super::super::limits()),
            Err(Error::Unsupported(_))
        ));
        assert_eq!(
            std::fs::read_to_string(root.join("sentinel")).unwrap(),
            "unchanged"
        );
        assert_eq!(std::fs::read_dir(root).unwrap().count(), 1);
    }
}
