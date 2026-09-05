use super::*;
use crate::QuiescentSource;

fn limits() -> Limits {
    Limits {
        max_entries: 64,
        max_file_bytes: 4096,
        max_total_bytes: 16384,
        max_manifest_bytes: 16384,
        max_path_bytes: 512,
        max_depth: 32,
    }
}
fn git_config() -> GitConfig {
    GitConfig {
        binary: "/usr/bin/git".into(),
        timeout: std::time::Duration::from_secs(5),
    }
}

#[test]
fn interrupted_capture_never_publishes_partial_or_reselects_frozen_source() {
    for point in [
        CapturePoint::Staged,
        CapturePoint::Frozen,
        CapturePoint::Published,
    ] {
        let temp = tempfile::TempDir::new().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        let result = std::process::Command::new("/usr/bin/git")
            .env_clear()
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(["init", "--quiet"])
            .arg(&source)
            .status()
            .unwrap();
        assert!(result.success());
        fs::write(source.join("notes"), b"original").unwrap();
        let root = temp.path().join("store");
        let store = ArtifactStore::open(&root, limits(), git_config()).unwrap();
        let request = || CaptureRequest {
            key: "capture-op",
            source: QuiescentSource {
                root: &source,
                boundary_id: "stopped",
            },
            outputs: &[],
        };
        let result = store.capture_inner(request(), |at| {
            if at == point {
                Err(Error::Io(std::io::Error::other(
                    "simulated interrupted publish",
                )))
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        let published: Vec<_> = fs::read_dir(root.join("snapshots"))
            .unwrap()
            .map(|e| e.unwrap())
            .collect();
        assert_eq!(
            published.len(),
            usize::from(point == CapturePoint::Published)
        );
        for entry in &published {
            store
                .open_snapshot(&entry.file_name().into_string().unwrap().parse().unwrap())
                .unwrap();
        }
        fs::write(source.join("notes"), b"changed!").unwrap();
        let store = ArtifactStore::open(&root, limits(), git_config()).unwrap();
        let receipt = store.capture(request()).unwrap();
        assert_eq!(receipt.replayed, point != CapturePoint::Staged);
        let destination = temp.path().join("repair");
        store
            .materialize_candidate(&receipt.snapshot, &destination)
            .unwrap();
        assert_eq!(
            fs::read(destination.join("notes")).unwrap(),
            if point == CapturePoint::Staged {
                b"changed!"
            } else {
                b"original"
            }
        );
        assert_eq!(fs::read_dir(root.join("staging")).unwrap().count(), 0);
    }
}

#[test]
fn startup_reclaims_only_unpublished_owned_temporary_directories() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path().join("store");
    ArtifactStore::open(&root, limits(), git_config()).unwrap();
    for prefix in ["capture-", "prepare-"] {
        let orphan = tempfile::Builder::new()
            .prefix(prefix)
            .rand_bytes(12)
            .tempdir_in(root.join("staging"))
            .unwrap()
            .keep();
        fs::write(orphan.join("partial"), b"unpublished").unwrap();
    }
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("valuable"), b"keep").unwrap();
    let orphan = tempfile::Builder::new()
        .prefix("capture-")
        .rand_bytes(12)
        .tempdir_in(root.join("staging"))
        .unwrap()
        .keep();
    std::os::unix::fs::symlink(&outside, orphan.join("link")).unwrap();
    ArtifactStore::open(&root, limits(), git_config()).unwrap();
    assert_eq!(fs::read_dir(root.join("staging")).unwrap().count(), 0);
    assert_eq!(fs::read(outside.join("valuable")).unwrap(), b"keep");
    fs::create_dir(root.join("staging/unknown-data")).unwrap();
    assert!(ArtifactStore::open(&root, limits(), git_config()).is_err());
    assert!(root.join("staging/unknown-data").is_dir());
}
