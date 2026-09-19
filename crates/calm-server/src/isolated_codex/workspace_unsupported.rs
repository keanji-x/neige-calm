//! Fail closed: the isolated workspace authority requires Linux openat2.
use crate::error::{CalmError, Result};
use std::{
    fs::File,
    path::{Path, PathBuf},
};

fn unsupported<T>() -> Result<T> {
    Err(CalmError::Conflict(
        "isolated workspaces require Linux openat2 support".into(),
    ))
}

pub(crate) fn prepare_root(_root: &Path) -> Result<()> {
    unsupported()
}

pub(crate) fn prepare(_root: &Path, _op_id: &str) -> Result<PathBuf> {
    unsupported()
}

pub(crate) fn open_retained(_root: &Path, _op_id: &str, _workspace: &Path) -> Result<File> {
    unsupported()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_workspace_does_not_create_or_open_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        assert!(
            prepare_root(&root)
                .unwrap_err()
                .to_string()
                .contains("Linux")
        );
        assert!(prepare(&root, "operation").is_err());
        assert!(!root.exists());
        // Even an existing readable directory cannot become workspace authority.
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("sentinel"), "unchanged").unwrap();
        assert!(prepare_root(&root).is_err());
        assert!(prepare(&root, "operation").is_err());
        assert!(open_retained(temp.path(), "workspace", &root).is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("sentinel")).unwrap(),
            "unchanged"
        );
        assert!(!root.join("operation").exists());
    }
}
