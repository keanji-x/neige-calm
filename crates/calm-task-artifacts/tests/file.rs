#![cfg(target_os = "linux")]
mod support;
use calm_task_artifacts::{
    ArtifactStore, CaptureRequest, Entry, Error, FileArtifactPath, FileCaptureRequest, Limits,
    QuiescentSource, SlotBinding,
};
use std::{
    fs::{self, File, OpenOptions},
    io::{Seek, SeekFrom},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};
use support::{git_config, limits, slot};

fn open(path: &Path) -> calm_task_artifacts::Result<File> {
    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOFOLLOW)
        .open(path)?)
}
fn request<'a>(path: &'a FileArtifactPath) -> FileCaptureRequest<'a> {
    FileCaptureRequest {
        key: "capture-op",
        boundary_id: "confirmed-stop-op",
        output: "result",
        path,
    }
}
fn path(value: &str) -> FileArtifactPath {
    FileArtifactPath::new(value, &limits()).unwrap()
}
fn binding(id: calm_task_artifacts::SnapshotId) -> SlotBinding {
    SlotBinding {
        snapshot: id,
        output: "result".into(),
        into: "inputs/a".into(),
    }
}

#[test]
fn file_capture_replays_deleted_source_without_invoking_opener() {
    let temp = tempfile::tempdir().unwrap();
    let store_root = temp.path().join("store");
    let store = ArtifactStore::open_files(&store_root, limits()).unwrap();
    let source = temp.path().join("result");
    fs::write(&source, b"42\n").unwrap();
    let path = path("result.json");
    let first = store
        .capture_file(request(&path), || open(&source))
        .unwrap();
    fs::write(&source, b"99\n").unwrap();
    fs::remove_file(&source).unwrap();
    let reopened = ArtifactStore::open_files(&store_root, limits()).unwrap();
    let replay = reopened
        .capture_file(request(&path), || panic!("frozen replay opened source"))
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(first.snapshot, replay.snapshot);
    assert_eq!(
        reopened
            .read_snapshot_file(&replay.snapshot, &path)
            .unwrap(),
        b"42\n"
    );
    let destination = temp.path().join("consumer");
    reopened
        .materialize(&[binding(replay.snapshot)], &destination)
        .unwrap();
    assert_eq!(
        fs::read(destination.join("inputs/a/result.json")).unwrap(),
        b"42\n"
    );
}

#[test]
fn file_capture_conflicts_on_changed_binding_and_git_contract() {
    let temp = tempfile::tempdir().unwrap();
    let store = ArtifactStore::open(&temp.path().join("store"), limits(), git_config()).unwrap();
    let source = temp.path().join("file");
    fs::write(&source, b"data").unwrap();
    let path = path("result.json");
    let other_path = FileArtifactPath::new("other.json", &limits()).unwrap();
    store
        .capture_file(request(&path), || open(&source))
        .unwrap();
    for changed in [
        FileCaptureRequest {
            boundary_id: "different-stop",
            ..request(&path)
        },
        FileCaptureRequest {
            output: "different-output",
            ..request(&path)
        },
        FileCaptureRequest {
            path: &other_path,
            ..request(&path)
        },
    ] {
        assert!(matches!(
            store.capture_file(changed, || panic!("conflict opened source")),
            Err(Error::Conflict)
        ));
    }
    assert!(matches!(
        store.capture(CaptureRequest {
            key: "capture-op",
            source: QuiescentSource {
                root: &temp.path().join("absent"),
                boundary_id: "confirmed-stop-op"
            },
            outputs: &[slot("result", &["result.json"])],
        }),
        Err(Error::Conflict)
    ));
}

#[test]
fn file_capture_uses_pinned_fd_full_bytes_and_only_declared_file() {
    let temp = tempfile::tempdir().unwrap();
    let store = ArtifactStore::open_files(&temp.path().join("store"), limits()).unwrap();
    let source = temp.path().join("workspace");
    fs::create_dir(&source).unwrap();
    fs::create_dir(source.join(".codex")).unwrap();
    fs::write(source.join(".codex/private"), b"private").unwrap();
    fs::write(source.join("huge"), vec![0; 5 * 1024 * 1024]).unwrap();
    nix::unistd::mkfifo(
        &source.join("unrelated-fifo"),
        nix::sys::stat::Mode::S_IRUSR,
    )
    .unwrap();
    let bytes = b"not JSON\0\xff\r\n";
    fs::write(source.join("result"), bytes).unwrap();
    fs::set_permissions(source.join("result"), fs::Permissions::from_mode(0o4755)).unwrap();
    let mut pinned = open(&source.join("result")).unwrap();
    pinned.seek(SeekFrom::Start(4)).unwrap();
    fs::rename(&source, temp.path().join("retained")).unwrap();
    fs::create_dir(&source).unwrap();
    fs::write(source.join("result"), b"replacement").unwrap();
    let path = path("out/nested/result.json");
    let capture = store.capture_file(request(&path), || Ok(pinned)).unwrap();
    let snapshot = store.open_snapshot(&capture.snapshot).unwrap();
    assert_eq!(snapshot.manifest().delivery_version, "regular-file-v1");
    assert_eq!(
        snapshot
            .manifest()
            .entries
            .iter()
            .map(Entry::path)
            .collect::<Vec<_>>(),
        ["out", "out/nested", "out/nested/result.json"]
    );
    assert!(snapshot.missing_outputs().is_empty());
    assert_eq!(
        store.read_snapshot_file(&capture.snapshot, &path).unwrap(),
        bytes
    );
    assert!(
        store
            .read_snapshot_file(
                &capture.snapshot,
                &FileArtifactPath::new("out", &limits()).unwrap()
            )
            .is_err()
    );
    let destination = temp.path().join("consumer");
    store
        .materialize(&[binding(capture.snapshot.clone())], &destination)
        .unwrap();
    let file = destination.join("inputs/a/out/nested/result.json");
    assert_eq!(fs::read(&file).unwrap(), bytes);
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o7777,
        0o700
    );
    assert!(matches!(
        store.materialize(&[binding(capture.snapshot)], &destination),
        Err(Error::DestinationExists(_))
    ));
    assert!(!destination.join("inputs/a/.codex").exists());
}

#[test]
fn file_capture_corruption_blocks_read_replay_and_materialization() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let store = ArtifactStore::open_files(&root, limits()).unwrap();
    let source = temp.path().join("source");
    fs::write(&source, b"correct").unwrap();
    let path = path("result.json");
    let capture = store
        .capture_file(request(&path), || open(&source))
        .unwrap();
    let snapshot = store.open_snapshot(&capture.snapshot).unwrap();
    let Entry::File { digest, .. } = &snapshot.manifest().entries[0] else {
        panic!("expected file")
    };
    let object = root
        .join("snapshots")
        .join(capture.snapshot.as_str())
        .join("objects")
        .join(digest.as_str());
    fs::write(&object, b"corrupt").unwrap();
    assert!(matches!(
        store.read_snapshot_file(&capture.snapshot, &path),
        Err(Error::Integrity(_))
    ));
    assert!(matches!(
        store.capture_file(request(&path), || panic!(
            "corrupt replay must not recapture"
        )),
        Err(Error::Integrity(_))
    ));
    let destination = temp.path().join("consumer");
    assert!(matches!(
        store.materialize(&[binding(capture.snapshot.clone())], &destination),
        Err(Error::Integrity(_))
    ));
    assert!(!destination.exists());
    fs::remove_file(object).unwrap();
    assert!(store.read_snapshot_file(&capture.snapshot, &path).is_err());
}

#[test]
fn file_capture_rejects_reserved_paths_and_invalid_binding_before_source_open() {
    for name in [
        "",
        "/absolute",
        "../escape",
        "a/../b",
        "a/./b",
        "a//b",
        "a\\b",
        "a/",
        ".git/config",
        ".gitmodules",
        ".codex/config",
        "out/.codex/value",
        "a\0b",
    ] {
        assert!(FileArtifactPath::new(name, &limits()).is_err(), "{name:?}");
    }
    let temp = tempfile::tempdir().unwrap();
    let store = ArtifactStore::open_files(&temp.path().join("store"), limits()).unwrap();
    let path = path("valid.json");
    for invalid in [
        FileCaptureRequest {
            key: "",
            ..request(&path)
        },
        FileCaptureRequest {
            boundary_id: "",
            ..request(&path)
        },
        FileCaptureRequest {
            output: "invalid/name",
            ..request(&path)
        },
    ] {
        assert!(matches!(
            store.capture_file(invalid, || panic!("invalid binding opened source")),
            Err(Error::Invalid(_))
        ));
    }
    assert_eq!(
        fs::read_dir(temp.path().join("store/captures"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn file_capture_rejects_nonregular_hardlinked_and_unsafe_descriptors() {
    for case in ["directory", "fifo", "hardlink", "blocking", "writable"] {
        let temp = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open_files(&temp.path().join("store"), limits()).unwrap();
        let source = temp.path().join("source");
        match case {
            "directory" => fs::create_dir(&source).unwrap(),
            "fifo" => nix::unistd::mkfifo(&source, nix::sys::stat::Mode::S_IRUSR).unwrap(),
            _ => fs::write(&source, b"data").unwrap(),
        }
        if case == "hardlink" {
            fs::hard_link(&source, temp.path().join("alias")).unwrap();
        }
        let result = store.capture_file(request(&path("result.json")), || match case {
            "blocking" => Ok(File::open(&source)?),
            "writable" => Ok(OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(nix::libc::O_NONBLOCK)
                .open(&source)?),
            _ => open(&source),
        });
        assert!(
            matches!(result, Err(Error::Unsupported(_)) | Err(Error::Invalid(_))),
            "{case}: {result:?}"
        );
        assert_eq!(
            fs::read_dir(temp.path().join("store/captures"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            fs::read_dir(temp.path().join("store/snapshots"))
                .unwrap()
                .count(),
            0
        );
    }
}

#[test]
fn file_capture_limits_apply_to_capture_reopen_and_read() {
    for case in ["file", "total", "entries", "path", "depth", "manifest"] {
        let temp = tempfile::tempdir().unwrap();
        let mut config = limits();
        match case {
            "file" => config.max_file_bytes = 3,
            "total" => config.max_total_bytes = 3,
            "entries" => config.max_entries = 1,
            "path" => config.max_path_bytes = 2,
            "depth" => config.max_depth = 1,
            "manifest" => config.max_manifest_bytes = 8,
            _ => unreachable!(),
        }
        let store = ArtifactStore::open_files(&temp.path().join("store"), config).unwrap();
        let source = temp.path().join("source");
        fs::write(&source, b"four").unwrap();
        let result = store.capture_file(request(&path("out/result.json")), || open(&source));
        assert!(matches!(result, Err(Error::Limit(_))), "{case}: {result:?}");
        assert_eq!(
            fs::read_dir(temp.path().join("store/captures"))
                .unwrap()
                .count(),
            0
        );
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let store = ArtifactStore::open_files(&root, limits()).unwrap();
    let source = temp.path().join("source");
    fs::write(&source, b"four").unwrap();
    let path = path("result.json");
    let capture = store
        .capture_file(request(&path), || open(&source))
        .unwrap();
    let smaller = ArtifactStore::open_files(
        &root,
        Limits {
            max_file_bytes: 3,
            ..limits()
        },
    )
    .unwrap();
    assert!(matches!(
        smaller.read_snapshot_file(&capture.snapshot, &path),
        Err(Error::Limit(_))
    ));
}

#[test]
fn file_capture_empty_file_is_valid_and_failed_open_can_retry() {
    let temp = tempfile::tempdir().unwrap();
    let store = ArtifactStore::open_files(&temp.path().join("store"), limits()).unwrap();
    let source = temp.path().join("missing");
    let path = path("empty.json");
    assert!(
        store
            .capture_file(request(&path), || open(&source))
            .is_err()
    );
    fs::write(&source, b"").unwrap();
    let result = store
        .capture_file(request(&path), || open(&source))
        .unwrap();
    assert!(!result.replayed);
    assert_eq!(
        store.read_snapshot_file(&result.snapshot, &path).unwrap(),
        b""
    );
}

#[test]
fn legacy_git_manifest_and_capture_fingerprint_remain_byte_compatible() {
    let rig = support::Rig::new();
    rig.write("result.json", b"42\n");
    let captured = rig.capture("legacy-key", &[slot("result", &["result.json"])]);
    assert_eq!(
        captured.snapshot.as_str(),
        "23ace2d1fc07576d728e5a99b953b2a4dc7448b13a902734d6c8fcedb4b47c97"
    );
    let record: serde_json::Value = serde_json::from_slice(&fs::read(rig.root.join(
        "captures/94eeb7bbe979dd0d2f0bb085172b76a1bc9e61789839480834a9bcb5aaf9c6ef/request.json"
    )).unwrap()).unwrap();
    assert_eq!(
        record,
        serde_json::json!({
            "version": "capture-v1",
            "key_digest": "94eeb7bbe979dd0d2f0bb085172b76a1bc9e61789839480834a9bcb5aaf9c6ef",
            "fingerprint": "3a466a44127122b99700d283b0fcb2899327c65c7ecbcb18ff14549e42ea0d9d",
            "snapshot": "23ace2d1fc07576d728e5a99b953b2a4dc7448b13a902734d6c8fcedb4b47c97"
        })
    );
    let files = ArtifactStore::open_files(&rig.root, limits()).unwrap();
    assert_eq!(
        files
            .read_snapshot_file(&captured.snapshot, &path("result.json"))
            .unwrap(),
        b"42\n"
    );
    files
        .materialize(&[binding(captured.snapshot)], &rig.destination("consumer"))
        .unwrap();
    assert!(matches!(
        files.capture(CaptureRequest {
            key: "legacy-key",
            source: QuiescentSource {
                root: &rig.source,
                boundary_id: "test-stopped-runtime-1"
            },
            outputs: &[slot("result", &["result.json"])],
        }),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        rig.store.capture_file(
            FileCaptureRequest {
                key: "legacy-key",
                boundary_id: "test-stopped-runtime-1",
                output: "result",
                path: &path("result.json"),
            },
            || panic!("Git binding cannot replay as ordinary file")
        ),
        Err(Error::Conflict)
    ));
}

#[test]
fn file_manifest_reopen_enforces_one_file_shape_and_version() {
    use sha2::{Digest as _, Sha256};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let store = ArtifactStore::open_files(&root, limits()).unwrap();
    let source = temp.path().join("source");
    fs::write(&source, b"data").unwrap();
    let receipt = store
        .capture_file(request(&path("result.json")), || open(&source))
        .unwrap();
    let snapshot = store.open_snapshot(&receipt.snapshot).unwrap();
    for case in [
        "extra-file",
        "extra-directory",
        "missing-file",
        "private-path",
        "extra-output",
        "unknown-version",
    ] {
        let mut manifest = snapshot.manifest().clone();
        match case {
            "extra-file" => {
                let mut extra = manifest.entries[0].clone();
                if let Entry::File { path, .. } = &mut extra {
                    *path = "unrelated".into();
                }
                manifest.entries.push(extra);
            }
            "extra-directory" => manifest.entries.push(Entry::Directory {
                path: "unrelated".into(),
            }),
            "missing-file" => manifest.entries.clear(),
            "private-path" => {
                if let Entry::File { path, .. } = &mut manifest.entries[0] {
                    *path = ".codex".into();
                }
                manifest.outputs[0].paths[0] = ".codex".into();
            }
            "extra-output" => manifest.outputs.push(slot("unused", &["unrelated"])),
            "unknown-version" => manifest.delivery_version = "regular-file-v2".into(),
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let id: calm_task_artifacts::SnapshotId =
            hex::encode(Sha256::digest(&bytes)).parse().unwrap();
        let location = root.join("snapshots").join(id.as_str());
        fs::create_dir(&location).unwrap();
        fs::write(location.join("manifest.json"), bytes).unwrap();
        // Valid content identity deliberately supplied; the manifest contract,
        // rather than a digest mismatch or absent object, must reject the shape.
        assert!(
            matches!(
                store.open_snapshot(&id),
                Err(Error::Integrity(_)) | Err(Error::Invalid(_)) | Err(Error::Unsupported(_))
            ),
            "{case}"
        );
    }
}
