#![cfg(target_os = "linux")]
mod support;
use calm_task_artifacts::{
    ArtifactStore, Error, FileArtifactPath, FileCaptureRequest, SlotBinding, SnapshotId,
};
use std::{
    fs,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink},
    path::PathBuf,
};
use support::limits;

struct Fixture {
    temp: tempfile::TempDir,
    /// Store, source and destination all live directly beneath this directory.
    base: PathBuf,
    store: ArtifactStore,
    id: SnapshotId,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().to_path_buf();
        Self::beneath(temp, base)
    }
    fn beneath(temp: tempfile::TempDir, base: PathBuf) -> Self {
        let store = ArtifactStore::open_files(&base.join("store"), limits()).unwrap();
        let source = base.join("source");
        fs::write(&source, b"original").unwrap();
        let path = FileArtifactPath::new("result.json", &limits()).unwrap();
        let receipt = store
            .capture_file(
                FileCaptureRequest {
                    key: "capture",
                    boundary_id: "stopped",
                    output: "result",
                    path: &path,
                },
                || {
                    Ok(fs::OpenOptions::new()
                        .read(true)
                        .custom_flags(nix::libc::O_NONBLOCK)
                        .open(&source)?)
                },
            )
            .unwrap();
        fs::remove_file(source).unwrap();
        Self {
            temp,
            base,
            store,
            id: receipt.snapshot,
        }
    }
    fn destination(&self) -> PathBuf {
        self.base.join("consumer")
    }
    fn bindings(&self) -> Vec<SlotBinding> {
        vec![SlotBinding {
            snapshot: self.id.clone(),
            output: "result".into(),
            into: "input".into(),
        }]
    }
    fn prepare(&self) {
        self.store
            .materialize(&self.bindings(), &self.destination())
            .unwrap();
    }
}

#[test]
fn materialized_reconciliation_reopens_exact_published_inputs_without_replacement() {
    let fixture = Fixture::new();
    let original = fixture
        .store
        .materialize(&fixture.bindings(), &fixture.destination())
        .unwrap();
    assert_eq!(
        fs::metadata(fixture.destination())
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o700
    );
    let file = fixture.destination().join("input/result.json");
    let inode = fs::metadata(&file).unwrap().ino();
    assert!(matches!(
        fixture
            .store
            .materialize(&fixture.bindings(), &fixture.destination()),
        Err(Error::DestinationExists(_))
    ));
    let reopened = ArtifactStore::open_files(&fixture.temp.path().join("store"), limits()).unwrap();
    let verified = reopened
        .verify_materialized(&fixture.bindings(), &fixture.destination())
        .unwrap();
    assert_eq!(verified.destination, original.destination);
    assert_eq!(verified.entries, original.entries);
    assert_eq!(fs::metadata(file).unwrap().ino(), inode);
}

/// #1636: mkdir(2) beneath a setgid directory yields `0o2700`, not the requested
/// `0o700` (CI's self-hosted `$RUNNER_TEMP` is such a directory). The store owns
/// the final mode of every directory it creates, so the exact-mode fence in
/// preparation and reconciliation passes wherever the store root lives.
#[test]
fn materialization_owns_exact_directory_modes_beneath_setgid_parent() {
    let temp = tempfile::tempdir().unwrap();
    let parent = temp.path().join("setgid-parent");
    fs::create_dir(&parent).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o2700)).unwrap();
    assert_eq!(
        fs::metadata(&parent).unwrap().mode() & 0o7777,
        0o2700,
        "fixture parent must carry the setgid bit"
    );
    let fixture = Fixture::beneath(temp, parent.clone());
    let destination = fixture.destination();
    let published = fixture
        .store
        .materialize(&fixture.bindings(), &destination)
        .unwrap();
    for directory in [destination.clone(), destination.join("input")] {
        assert_eq!(
            fs::metadata(&directory).unwrap().mode() & 0o7777,
            0o700,
            "{}",
            directory.display()
        );
    }
    let reopened = ArtifactStore::open_files(&parent.join("store"), limits()).unwrap();
    let verified = reopened
        .verify_materialized(&fixture.bindings(), &destination)
        .unwrap();
    assert_eq!(verified.entries, published.entries);
    assert_eq!(
        fs::read(destination.join("input/result.json")).unwrap(),
        b"original"
    );
}

#[test]
fn materialized_reconciliation_refuses_inventory_links_types_and_metadata_drift() {
    for case in [
        "bytes",
        "extra-file",
        "extra-directory",
        "missing-file",
        "symlink",
        "hardlink",
        "directory-symlink",
        "fifo",
        "file-mode",
        "directory-mode",
        "root-mode",
        "special-mode",
    ] {
        let fixture = Fixture::new();
        fixture.prepare();
        let destination = fixture.destination();
        let file = destination.join("input/result.json");
        let outside = fixture.temp.path().join("outside");
        match case {
            "bytes" => fs::write(&file, b"modified").unwrap(),
            "extra-file" => fs::write(destination.join("input/extra"), b"extra").unwrap(),
            "extra-directory" => fs::create_dir(destination.join("extra")).unwrap(),
            "missing-file" => fs::remove_file(&file).unwrap(),
            "symlink" => {
                fs::rename(&file, &outside).unwrap();
                symlink(&outside, &file).unwrap();
            }
            "hardlink" => fs::hard_link(&file, &outside).unwrap(),
            "directory-symlink" => {
                fs::rename(destination.join("input"), &outside).unwrap();
                symlink(&outside, destination.join("input")).unwrap();
            }
            "fifo" => {
                fs::remove_file(&file).unwrap();
                nix::unistd::mkfifo(&file, nix::sys::stat::Mode::S_IRUSR).unwrap();
            }
            "file-mode" => fs::set_permissions(&file, fs::Permissions::from_mode(0o700)).unwrap(),
            "directory-mode" => {
                fs::set_permissions(destination.join("input"), fs::Permissions::from_mode(0o755))
                    .unwrap()
            }
            "root-mode" => {
                fs::set_permissions(&destination, fs::Permissions::from_mode(0o755)).unwrap()
            }
            "special-mode" => {
                fs::set_permissions(&file, fs::Permissions::from_mode(0o4600)).unwrap()
            }
            _ => unreachable!(),
        }
        let root_inode = fs::metadata(&destination).unwrap().ino();
        assert!(
            fixture
                .store
                .verify_materialized(&fixture.bindings(), &destination)
                .is_err(),
            "{case}"
        );
        assert_eq!(
            fs::metadata(&destination).unwrap().ino(),
            root_inode,
            "{case}"
        );
        if case == "bytes" {
            assert_eq!(fs::read(&file).unwrap(), b"modified");
        }
        if case == "missing-file" {
            assert!(!file.exists());
        }
        if case == "extra-file" {
            assert_eq!(fs::read(destination.join("input/extra")).unwrap(), b"extra");
        }
    }
}

#[test]
fn materialized_reconciliation_requires_exact_bindings_and_retained_snapshot() {
    let fixture = Fixture::new();
    fixture.prepare();
    let original = fixture.bindings().remove(0);
    for bindings in [
        vec![SlotBinding {
            into: "other".into(),
            ..original.clone()
        }],
        vec![SlotBinding {
            output: "other".into(),
            ..original.clone()
        }],
        vec![original.clone(), original.clone()],
    ] {
        assert!(
            fixture
                .store
                .verify_materialized(&bindings, &fixture.destination())
                .is_err()
        );
    }
    // Correctly hashed, different immutable version is still the wrong input.
    let source = fixture.temp.path().join("changed");
    fs::write(&source, b"modified").unwrap();
    let receipt = fixture
        .store
        .capture_file(
            FileCaptureRequest {
                key: "other-capture",
                boundary_id: "other-stop",
                output: "result",
                path: &FileArtifactPath::new("result.json", &limits()).unwrap(),
            },
            || {
                Ok(fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(nix::libc::O_NONBLOCK)
                    .open(&source)?)
            },
        )
        .unwrap();
    assert!(matches!(
        fixture.store.verify_materialized(
            &[SlotBinding {
                snapshot: receipt.snapshot,
                ..original
            }],
            &fixture.destination()
        ),
        Err(Error::Integrity(_))
    ));
    fs::remove_dir_all(
        fixture
            .temp
            .path()
            .join("store/snapshots")
            .join(fixture.id.as_str()),
    )
    .unwrap();
    assert!(
        fixture
            .store
            .verify_materialized(&fixture.bindings(), &fixture.destination())
            .is_err()
    );
    assert_eq!(
        fs::read(fixture.destination().join("input/result.json")).unwrap(),
        b"original"
    );
}

#[test]
fn materialized_reconciliation_never_creates_or_follows_destination() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .store
            .verify_materialized(&fixture.bindings(), &fixture.destination())
            .is_err()
    );
    assert!(!fixture.destination().exists());
    fixture.prepare();
    let retained = fixture.temp.path().join("retained");
    fs::rename(fixture.destination(), &retained).unwrap();
    symlink(&retained, fixture.destination()).unwrap();
    assert!(
        fixture
            .store
            .verify_materialized(&fixture.bindings(), &fixture.destination())
            .is_err()
    );
    assert!(
        fs::symlink_metadata(fixture.destination())
            .unwrap()
            .is_symlink()
    );
    assert_eq!(
        fs::read(retained.join("input/result.json")).unwrap(),
        b"original"
    );
}
