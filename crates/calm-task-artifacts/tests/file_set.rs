#![cfg(target_os = "linux")]
mod support;
use calm_task_artifacts::{
    ArtifactStore, Entry, Error, FileArtifactPath, FileCaptureRequest, FileSetCaptureRequest,
    Limits, SlotBinding,
};
use std::{
    fs::{self, File, OpenOptions},
    io::{Seek, SeekFrom},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};
use support::limits;

fn paths(names: &[&str]) -> Vec<FileArtifactPath> {
    names
        .iter()
        .map(|p| FileArtifactPath::new(*p, &limits()).unwrap())
        .collect()
}
fn request(paths: &[FileArtifactPath]) -> FileSetCaptureRequest<'_> {
    FileSetCaptureRequest {
        key: "set",
        boundary_id: "stopped",
        output: "result",
        paths,
    }
}
fn open(path: &Path) -> calm_task_artifacts::Result<File> {
    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOFOLLOW)
        .open(path)?)
}
fn unpublished(root: &Path) {
    for dir in ["captures", "snapshots", "staging"] {
        assert_eq!(fs::read_dir(root.join(dir)).unwrap().count(), 0, "{dir}");
    }
}

#[test]
fn file_set_exact_bytes_modes_inventory_and_deleted_source_replay() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let store = ArtifactStore::open_files(&root, limits()).unwrap();
    let source = temp.path().join("source");
    fs::create_dir_all(source.join("out/nested")).unwrap();
    fs::write(source.join("out/nested/b"), b"\0\xff\r\n").unwrap();
    fs::write(source.join("out/a"), b"plain").unwrap();
    fs::write(source.join("empty"), b"").unwrap();
    fs::write(source.join("unrelated"), b"not captured").unwrap();
    fs::create_dir(source.join(".codex")).unwrap();
    fs::write(source.join(".codex/private"), b"private").unwrap();
    nix::unistd::mkfifo(&source.join("fifo"), nix::sys::stat::Mode::S_IRUSR).unwrap();
    fs::set_permissions(
        source.join("out/nested/b"),
        fs::Permissions::from_mode(0o4755),
    )
    .unwrap();
    let mut declared = paths(&["out/nested/b", "out/a", "empty"]);
    let mut opened = Vec::new();
    let first = store
        .capture_files(request(&declared), |p| {
            opened.push(p.as_str().to_owned());
            let mut file = open(&source.join(p.as_str()))?;
            file.seek(SeekFrom::Start(2))?;
            Ok(file)
        })
        .unwrap();
    assert!(!first.replayed);
    assert!(first.missing_outputs.is_empty());
    assert_eq!(opened, ["empty", "out/a", "out/nested/b"]);
    let snapshot = store.open_snapshot(&first.snapshot).unwrap();
    assert_eq!(snapshot.manifest().delivery_version, "regular-file-set-v1");
    assert_eq!(
        snapshot
            .manifest()
            .entries
            .iter()
            .map(Entry::path)
            .collect::<Vec<_>>(),
        ["empty", "out", "out/a", "out/nested", "out/nested/b"]
    );
    assert_eq!(snapshot.manifest().outputs[0].paths, opened);
    let manifest_bytes = fs::read(
        root.join("snapshots")
            .join(first.snapshot.as_str())
            .join("manifest.json"),
    )
    .unwrap();
    assert_eq!(
        manifest_bytes,
        serde_json::to_vec(snapshot.manifest()).unwrap()
    );
    fs::remove_dir_all(source).unwrap();
    declared.reverse();
    let reopened = ArtifactStore::open_files(&root, limits()).unwrap();
    let replay = reopened
        .capture_files(request(&declared), |_| panic!("replay opened source"))
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(first.snapshot, replay.snapshot);
    for (name, expected) in [
        ("empty", b"".as_slice()),
        ("out/a", b"plain"),
        ("out/nested/b", b"\0\xff\r\n"),
    ] {
        assert_eq!(
            reopened
                .read_snapshot_file(&first.snapshot, &paths(&[name])[0])
                .unwrap(),
            expected
        );
    }
    let bindings = [SlotBinding {
        snapshot: first.snapshot.clone(),
        output: "result".into(),
        into: "input".into(),
    }];
    let destination = temp.path().join("consumer");
    reopened.materialize(&bindings, &destination).unwrap();
    reopened
        .verify_materialized(&bindings, &destination)
        .unwrap();
    for (name, mode) in [("empty", 0o600), ("out/a", 0o600), ("out/nested/b", 0o700)] {
        assert_eq!(
            fs::metadata(destination.join("input").join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            mode
        );
    }
    let candidate = temp.path().join("candidate");
    reopened
        .materialize_candidate(&first.snapshot, &candidate)
        .unwrap();
    assert!(!candidate.join("unrelated").exists());
}

#[test]
fn file_set_order_independent_identity_and_changed_contract_conflicts() {
    let temp = tempfile::tempdir().unwrap();
    let store = ArtifactStore::open_files(&temp.path().join("store"), limits()).unwrap();
    let source = temp.path().join("source");
    fs::write(&source, b"same").unwrap();
    let a = paths(&["b", "a"]);
    let b = paths(&["a", "b"]);
    let first = store.capture_files(request(&a), |_| open(&source)).unwrap();
    let second = store
        .capture_files(
            FileSetCaptureRequest {
                key: "another",
                ..request(&b)
            },
            |_| open(&source),
        )
        .unwrap();
    assert_eq!(first.snapshot, second.snapshot);
    for changed in [
        FileSetCaptureRequest {
            boundary_id: "other",
            ..request(&a)
        },
        FileSetCaptureRequest {
            output: "other",
            ..request(&a)
        },
        request(&a[..1]),
    ] {
        assert!(matches!(
            store.capture_files(changed, |_| panic!("conflict opened")),
            Err(Error::Conflict)
        ));
    }
    assert!(matches!(
        store.capture_file(
            FileCaptureRequest {
                key: "set",
                boundary_id: "stopped",
                output: "result",
                path: &a[0],
            },
            || panic!("cross-contract conflict opened")
        ),
        Err(Error::Conflict)
    ));
}

#[test]
fn file_set_invalid_lists_and_store_path_limits_never_open() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let store = ArtifactStore::open_files(&root, limits()).unwrap();
    for names in [vec![], vec!["a", "a"], vec!["a/b", "a-", "a"]] {
        assert!(matches!(
            store.capture_files(request(&paths(&names)), |_| panic!("invalid opened")),
            Err(Error::Invalid(_))
        ));
        unpublished(&root);
    }
    for name in [
        ".codex/private",
        "a/.codex/b",
        ".git/config",
        "../a",
        "a//b",
    ] {
        assert!(FileArtifactPath::new(name, &limits()).is_err());
    }
    for config in [
        Limits {
            max_entries: 2,
            ..limits()
        },
        Limits {
            max_path_bytes: 2,
            ..limits()
        },
        Limits {
            max_depth: 1,
            ..limits()
        },
        Limits {
            max_manifest_bytes: 8,
            ..limits()
        },
    ] {
        let smaller = ArtifactStore::open_files(&root, config).unwrap();
        assert!(matches!(
            smaller.capture_files(request(&paths(&["out/a", "out/b"])), |_| panic!(
                "limit opened"
            )),
            Err(Error::Limit(_))
        ));
        unpublished(&root);
    }
}

#[test]
fn file_set_late_open_failure_publishes_nothing_and_retries_all_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let store = ArtifactStore::open_files(&root, limits()).unwrap();
    let source = temp.path().join("source");
    fs::write(&source, b"old").unwrap();
    let declared = paths(&["a", "b"]);
    let mut opened = Vec::new();
    assert!(matches!(
        store.capture_files(request(&declared), |p| {
            opened.push(p.as_str().to_owned());
            open(if p.as_str() == "a" {
                &source
            } else {
                Path::new("/nonexistent-neige-fileset-source")
            })
        }),
        Err(Error::Io(_))
    ));
    assert_eq!(opened, ["a", "b"]);
    unpublished(&root);
    fs::write(&source, b"new").unwrap();
    let receipt = store
        .capture_files(request(&declared), |_| open(&source))
        .unwrap();
    assert!(!receipt.replayed);
    for p in &declared {
        assert_eq!(
            store.read_snapshot_file(&receipt.snapshot, p).unwrap(),
            b"new"
        );
    }
}

#[test]
fn file_set_late_unsafe_descriptor_refuses_entire_snapshot() {
    for case in [
        "directory",
        "fifo",
        "hardlink",
        "blocking",
        "writable",
        "opath",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let store = ArtifactStore::open_files(&root, limits()).unwrap();
        let good = temp.path().join("good");
        let bad = temp.path().join("bad");
        fs::write(&good, b"good").unwrap();
        match case {
            "directory" => fs::create_dir(&bad).unwrap(),
            "fifo" => nix::unistd::mkfifo(&bad, nix::sys::stat::Mode::S_IRUSR).unwrap(),
            _ => fs::write(&bad, b"bad").unwrap(),
        }
        if case == "hardlink" {
            fs::hard_link(&bad, temp.path().join("alias")).unwrap();
        }
        let result = store.capture_files(request(&paths(&["a", "b"])), |p| {
            if p.as_str() == "a" {
                return open(&good);
            }
            match case {
                "blocking" => Ok(File::open(&bad)?),
                "writable" => Ok(OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(nix::libc::O_NONBLOCK)
                    .open(&bad)?),
                "opath" => Ok(OpenOptions::new()
                    .read(true)
                    .custom_flags(nix::libc::O_PATH | nix::libc::O_NONBLOCK)
                    .open(&bad)?),
                _ => open(&bad),
            }
        });
        assert!(
            matches!(result, Err(Error::Invalid(_)) | Err(Error::Unsupported(_))),
            "{case}: {result:?}"
        );
        unpublished(&root);
    }
}

#[test]
fn file_set_aggregate_bytes_and_shared_parents_obey_exact_limits() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::write(&source, b"four").unwrap();
    let declared = paths(&["out/a", "out/b"]);
    for (case, config) in [
        (
            "file",
            Limits {
                max_file_bytes: 3,
                ..limits()
            },
        ),
        (
            "total",
            Limits {
                max_total_bytes: 7,
                ..limits()
            },
        ),
        (
            "parents",
            Limits {
                max_entries: 2,
                ..limits()
            },
        ),
        (
            "manifest",
            Limits {
                max_manifest_bytes: 400,
                ..limits()
            },
        ),
    ] {
        let root = temp.path().join(case);
        let store = ArtifactStore::open_files(&root, config).unwrap();
        assert!(
            matches!(
                store.capture_files(request(&declared), |_| open(&source)),
                Err(Error::Limit(_))
            ),
            "{case}"
        );
        unpublished(&root);
    }
    let root = temp.path().join("exact");
    let store = ArtifactStore::open_files(
        &root,
        Limits {
            max_entries: 3,
            max_file_bytes: 4,
            max_total_bytes: 8,
            ..limits()
        },
    )
    .unwrap();
    let first = store
        .capture_files(request(&declared), |_| open(&source))
        .unwrap();
    for config in [
        Limits {
            max_total_bytes: 7,
            ..limits()
        },
        Limits {
            max_entries: 2,
            ..limits()
        },
    ] {
        let smaller = ArtifactStore::open_files(&root, config).unwrap();
        assert!(matches!(
            smaller.open_snapshot(&first.snapshot),
            Err(Error::Limit(_))
        ));
    }
    let empty = temp.path().join("empty");
    fs::write(&empty, b"").unwrap();
    let full = paths(&["a", "b", "z"]);
    let exact = ArtifactStore::open_files(
        &temp.path().join("zero-remaining"),
        Limits {
            max_total_bytes: 8,
            ..limits()
        },
    )
    .unwrap();
    exact
        .capture_files(request(&full), |p| {
            open(if p.as_str() == "z" { &empty } else { &source })
        })
        .unwrap();
}

#[test]
fn file_set_manifest_requires_exact_declared_files_and_parents() {
    use sha2::{Digest as _, Sha256};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let store = ArtifactStore::open_files(&root, limits()).unwrap();
    let source = temp.path().join("source");
    fs::write(&source, b"data").unwrap();
    let captured = store
        .capture_files(request(&paths(&["out/a", "out/b"])), |_| open(&source))
        .unwrap();
    let snapshot = store.open_snapshot(&captured.snapshot).unwrap();
    for case in [
        "extra-file",
        "extra-parent",
        "missing-file",
        "missing-parent",
        "directory-leaf",
        "private",
        "duplicate",
        "prefix",
        "extra-output",
        "empty-output",
        "no-output",
        "order",
    ] {
        let mut manifest = snapshot.manifest().clone();
        match case {
            "extra-file" => {
                let mut extra = manifest.entries[1].clone();
                if let Entry::File { path, .. } = &mut extra {
                    *path = "out/c".into();
                }
                manifest.entries.push(extra);
            }
            "extra-parent" => manifest.entries.push(Entry::Directory { path: "z".into() }),
            "missing-file" => {
                manifest.entries.pop();
            }
            "missing-parent" => {
                manifest.entries.remove(0);
            }
            "directory-leaf" => {
                manifest.entries[1] = Entry::Directory {
                    path: "out/a".into(),
                }
            }
            "private" => {
                manifest.entries.insert(
                    0,
                    Entry::Directory {
                        path: ".codex".into(),
                    },
                );
                let mut extra = manifest.entries[2].clone();
                if let Entry::File { path, .. } = &mut extra {
                    *path = ".codex/private".into();
                }
                manifest.entries.insert(1, extra);
                manifest.outputs[0].paths.insert(0, ".codex/private".into());
            }
            "duplicate" => manifest.outputs[0].paths.push("out/b".into()),
            "prefix" => manifest.outputs[0].paths.insert(0, "out".into()),
            "extra-output" => manifest.outputs.push(support::slot("unused", &["z"])),
            "empty-output" => manifest.outputs[0].paths.clear(),
            "no-output" => manifest.outputs.clear(),
            "order" => manifest.entries.swap(1, 2),
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let id: calm_task_artifacts::SnapshotId =
            hex::encode(Sha256::digest(&bytes)).parse().unwrap();
        let location = root.join("snapshots").join(id.as_str());
        fs::create_dir(&location).unwrap();
        fs::write(location.join("manifest.json"), bytes).unwrap();
        // Hash-valid encoding: shape rejection must precede absent object reads.
        assert!(
            matches!(
                store.open_snapshot(&id),
                Err(Error::Integrity(_)) | Err(Error::Invalid(_))
            ),
            "{case}"
        );
    }
}

#[test]
fn file_set_extension_preserves_single_file_manifest_and_request_bytes() {
    use sha2::{Digest as _, Sha256};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let store = ArtifactStore::open_files(&root, limits()).unwrap();
    let source = temp.path().join("source");
    fs::write(&source, b"42\n").unwrap();
    let path = paths(&["result.json"]);
    let captured = store
        .capture_file(
            FileCaptureRequest {
                key: "legacy-file",
                boundary_id: "stopped",
                output: "result",
                path: &path[0],
            },
            || open(&source),
        )
        .unwrap();
    let expected = concat!(
        "{\"version\":\"file-manifest-v1\",\"delivery_version\":\"regular-file-v1\",",
        "\"entries\":[{\"kind\":\"file\",\"path\":\"result.json\",",
        "\"digest\":\"084c799cd551dd1d8d5c5f9a5d593b2e931f5e36122ee5c793c1d08a19839cc0\",",
        "\"bytes\":3,\"executable\":false}],\"outputs\":[{\"name\":\"result\",\"paths\":[\"result.json\"]}]}"
    );
    assert_eq!(
        fs::read(
            root.join("snapshots")
                .join(captured.snapshot.as_str())
                .join("manifest.json")
        )
        .unwrap(),
        expected.as_bytes()
    );
    assert_eq!(
        captured.snapshot.as_str(),
        hex::encode(Sha256::digest(expected))
    );
    let record: serde_json::Value = serde_json::from_slice(
        &fs::read(
            root.join("captures")
                .join(hex::encode(Sha256::digest(b"legacy-file")))
                .join("request.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        record["fingerprint"],
        hex::encode(Sha256::digest(
            br#"["file-capture-v1","stopped",[{"name":"result","paths":["result.json"]}]]"#
        ))
    );
    assert!(
        store
            .capture_file(
                FileCaptureRequest {
                    key: "legacy-file",
                    boundary_id: "stopped",
                    output: "result",
                    path: &path[0],
                },
                || panic!("legacy replay opened")
            )
            .unwrap()
            .replayed
    );
}

#[test]
fn file_set_cumulative_byte_cap_stops_before_opening_later_sources() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let store = ArtifactStore::open_files(
        &root,
        Limits {
            max_file_bytes: 4,
            max_total_bytes: 7,
            ..limits()
        },
    )
    .unwrap();
    let source = temp.path().join("source");
    fs::write(&source, b"four").unwrap();
    let declared = paths(&["a", "b", "c"]);
    let mut opened = Vec::new();
    let result = store.capture_files(request(&declared), |path| {
        opened.push(path.as_str().to_owned());
        open(&source)
    });
    assert!(matches!(result, Err(Error::Limit(_))));
    assert_eq!(
        opened,
        ["a", "b"],
        "the cumulative streaming cap must reject b before opening c"
    );
    unpublished(&root);
}
