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

struct CaptureFixture {
    temp: tempfile::TempDir,
    source: PathBuf,
    root: PathBuf,
    store: ArtifactStore,
}
impl CaptureFixture {
    fn new() -> Self {
        let temp = tempfile::TempDir::new().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        assert!(
            std::process::Command::new("/usr/bin/git")
                .env_clear()
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args(["init", "--quiet"])
                .arg(&source)
                .status()
                .unwrap()
                .success()
        );
        fs::write(source.join("notes"), b"original").unwrap();
        let root = temp.path().join("store");
        let store = ArtifactStore::open(&root, limits(), git_config()).unwrap();
        Self {
            temp,
            source,
            root,
            store,
        }
    }
    fn request(&self, key: &'static str) -> CaptureRequest<'_> {
        CaptureRequest {
            key,
            source: QuiescentSource {
                root: &self.source,
                boundary_id: "stopped",
            },
            outputs: &[],
        }
    }
    fn request_dir(&self, key: &str) -> PathBuf {
        self.root
            .join("captures")
            .join(Digest::of(key.as_bytes()).as_str())
    }
    fn record(&self, key: &str) -> CaptureRecord {
        serde_json::from_slice(&fs::read(self.request_dir(key).join("request.json")).unwrap())
            .unwrap()
    }
    fn assert_original(&self, receipt: &CaptureReceipt) {
        let destination = self.temp.path().join("restored");
        self.store
            .materialize_candidate(&receipt.snapshot, &destination)
            .unwrap();
        assert_eq!(fs::read(destination.join("notes")).unwrap(), b"original");
    }
    fn interrupt_duplicate_cleanup(&self) -> SnapshotId {
        let original = self.store.capture(self.request("first")).unwrap();
        let staged = self.request_dir("second").join("snapshot");
        let mut removed = false;
        let result = self.store.capture_inner(self.request("second"), |point| {
            if point == CapturePoint::RedundantCleanup {
                let object = fs::read_dir(staged.join("objects"))?
                    .next()
                    .unwrap()?
                    .path();
                fs::remove_file(object)?;
                removed = true;
                return Err(injected_io_error());
            }
            Ok(())
        });
        assert!(result.is_err());
        assert!(
            removed,
            "must interrupt actual redundant cleanup after removing an object"
        );
        assert!(staged.is_dir());
        assert_eq!(fs::read_dir(staged.join("objects")).unwrap().count(), 0);
        assert_eq!(self.record("second").snapshot, original.snapshot);
        original.snapshot
    }
}

fn injected_io_error() -> Error {
    Error::Io(std::io::Error::other("injected filesystem interruption"))
}
fn fail_sync<T>(path: PathBuf, operation: impl FnOnce() -> T) -> T {
    disk::faults::with_sync(
        move |at| {
            if at == path {
                Err(injected_io_error())
            } else {
                Ok(())
            }
        },
        operation,
    )
}

#[test]
fn capture_replay_repairs_interrupted_key_sync_before_publication() {
    for parent in ["captures", "staging"] {
        let fixture = CaptureFixture::new();
        let first = fail_sync(fixture.root.join(parent), || {
            fixture.store.capture(fixture.request("key"))
        });
        assert!(matches!(first, Err(Error::Io(_))));
        let record = fixture.record("key");
        assert!(fixture.request_dir("key").join("snapshot").is_dir());
        assert_eq!(
            fs::read_dir(fixture.root.join("snapshots"))
                .unwrap()
                .count(),
            0
        );
        fs::write(fixture.source.join("notes"), b"changed!").unwrap();
        let replay = fail_sync(fixture.root.join(parent), || {
            fixture.store.capture(fixture.request("key"))
        });
        assert!(
            matches!(replay, Err(Error::Io(_))),
            "replay must repair {parent} sync before publication: {replay:?}"
        );
        assert_eq!(
            fs::read_dir(fixture.root.join("snapshots"))
                .unwrap()
                .count(),
            0
        );
        fs::remove_dir_all(&fixture.source).unwrap();
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let observed = seen.clone();
        let receipt = disk::faults::with_sync(
            move |at| {
                observed.borrow_mut().push(at.to_path_buf());
                Ok(())
            },
            || fixture.store.capture(fixture.request("key")),
        )
        .unwrap();
        assert_eq!(
            &seen.borrow()[..2],
            &[fixture.root.join("captures"), fixture.root.join("staging")]
        );
        assert!(receipt.replayed);
        assert_eq!(receipt.snapshot, record.snapshot);
        fixture.assert_original(&receipt);
    }
}

#[test]
fn capture_replay_requires_key_sync_even_after_publication() {
    for parent in ["captures", "staging"] {
        let fixture = CaptureFixture::new();
        let first = fixture.store.capture(fixture.request("key")).unwrap();
        fs::remove_dir_all(&fixture.source).unwrap();
        let replay = fail_sync(fixture.root.join(parent), || {
            fixture.store.capture(fixture.request("key"))
        });
        assert!(
            matches!(replay, Err(Error::Io(_))),
            "published replay must not acknowledge failed {parent} sync: {replay:?}"
        );
        fixture.store.open_snapshot(&first.snapshot).unwrap();
        let receipt = fixture.store.capture(fixture.request("key")).unwrap();
        assert!(receipt.replayed);
        assert_eq!(receipt.snapshot, first.snapshot);
        fixture.assert_original(&receipt);
    }
}

#[test]
fn open_rejects_initialized_store_missing_control_directory() {
    for name in ["captures", "staging", "snapshots"] {
        let fixture = CaptureFixture::new();
        let first = fixture.store.capture(fixture.request("key")).unwrap();
        let missing = fixture.root.join(name);
        fs::rename(&missing, fixture.temp.path().join("lost-control-directory")).unwrap();
        fs::write(fixture.source.join("notes"), b"changed!").unwrap();
        assert_eq!(fs::read(fixture.root.join("FORMAT")).unwrap(), FORMAT);
        let reopened = ArtifactStore::open(&fixture.root, limits(), git_config());
        assert!(
            matches!(reopened, Err(Error::Integrity(_))),
            "initialized store must reject missing {name}: {reopened:?}"
        );
        assert!(!missing.exists(), "must not recreate lost metadata");
        if name != "snapshots" {
            fixture.store.open_snapshot(&first.snapshot).unwrap();
            assert_eq!(
                fs::read_dir(fixture.root.join("snapshots"))
                    .unwrap()
                    .count(),
                1
            );
        }
    }
}

#[test]
fn open_rejects_incomplete_initialization_before_format() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path().join("store");
    let opened = fail_sync(root.clone(), || {
        ArtifactStore::open(&root, limits(), git_config())
    });
    assert!(matches!(opened, Err(Error::Io(_))));
    assert!(
        !root.join("FORMAT").exists(),
        "FORMAT cannot precede the complete control layout"
    );
    assert!(root.join("staging").is_dir());
    assert!(ArtifactStore::open(&root, limits(), git_config()).is_err());
    assert!(
        !root.join("FORMAT").exists(),
        "partial initialization is not a fresh empty store"
    );
}

#[test]
fn open_rechecks_format_commit_sync_before_accepting_store() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path().join("store");
    let fail_commit = || {
        let watched = root.clone();
        disk::faults::with_sync(
            move |at| {
                if at == watched && watched.join("FORMAT").exists() {
                    Err(injected_io_error())
                } else {
                    Ok(())
                }
            },
            || ArtifactStore::open(&root, limits(), git_config()),
        )
    };
    assert!(matches!(fail_commit(), Err(Error::Io(_))));
    for name in ["staging", "captures", "snapshots"] {
        assert!(
            root.join(name).is_dir(),
            "FORMAT must only appear after {name} exists"
        );
    }
    assert!(
        matches!(fail_commit(), Err(Error::Io(_))),
        "reopen must finish the uncertain FORMAT commit"
    );
    ArtifactStore::open(&root, limits(), git_config()).unwrap();
}

#[test]
fn duplicate_cleanup_replays_after_partial_removal() {
    let fixture = CaptureFixture::new();
    let original = fixture.interrupt_duplicate_cleanup();
    let request_bytes = fs::read(fixture.request_dir("second").join("request.json")).unwrap();
    fs::remove_dir_all(&fixture.source).unwrap();
    let receipt = fixture.store.capture(fixture.request("second")).unwrap();
    assert!(receipt.replayed);
    assert_eq!(receipt.snapshot, original);
    assert!(!fixture.request_dir("second").join("snapshot").exists());
    assert_eq!(
        fs::read(fixture.request_dir("second").join("request.json")).unwrap(),
        request_bytes
    );
    fixture.assert_original(&receipt);
}

#[test]
fn duplicate_cleanup_preserves_stage_until_published_sync_succeeds() {
    let fixture = CaptureFixture::new();
    let original = fixture.store.capture(fixture.request("first")).unwrap();
    let staged = fixture.request_dir("second").join("snapshot");
    for _ in 0..2 {
        let result = fail_sync(fixture.root.join("snapshots"), || {
            fixture.store.capture(fixture.request("second"))
        });
        assert!(matches!(result, Err(Error::Io(_))));
        assert!(
            staged.is_dir(),
            "redundant bytes cannot be removed before canonical publication is durable"
        );
        fixture
            .store
            .load_snapshot(&staged, &original.snapshot)
            .unwrap();
        fs::write(fixture.source.join("notes"), b"changed!").unwrap();
    }
    fs::remove_dir_all(&fixture.source).unwrap();
    let receipt = fixture.store.capture(fixture.request("second")).unwrap();
    assert!(receipt.replayed);
    assert_eq!(receipt.snapshot, original.snapshot);
    fixture.assert_original(&receipt);
}

#[test]
fn duplicate_cleanup_replay_refuses_corrupt_or_missing_canonical_copy() {
    for damage in ["corrupt", "missing", "missing-both"] {
        let fixture = CaptureFixture::new();
        let original = fixture.interrupt_duplicate_cleanup();
        let published = fixture.store.snapshot_path(&original);
        if damage == "corrupt" {
            let manifest = published.join("manifest.json");
            let length = fs::metadata(&manifest).unwrap().len();
            fs::write(manifest, vec![b'x'; length as usize]).unwrap();
        } else {
            fs::remove_dir_all(published).unwrap();
            if damage == "missing-both" {
                fs::remove_dir_all(fixture.request_dir("second").join("snapshot")).unwrap();
            }
        }
        fs::write(fixture.source.join("notes"), b"changed!").unwrap();
        assert!(fixture.store.capture(fixture.request("second")).is_err());
        assert_eq!(fixture.record("second").snapshot, original);
    }
}
