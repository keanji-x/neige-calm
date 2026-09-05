#![cfg(target_os = "linux")]
mod support;
use calm_task_artifacts::{
    ArtifactStore, CaptureRequest, Digest, Entry, Error, QuiescentSource, SlotBinding,
};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    sync::Arc,
};
use support::*;

#[test]
fn sealed_candidate_survives_source_change_and_deletion() {
    let rig = Rig::new();
    rig.write("notes.txt", b"retained untracked work\0\xff");
    rig.write(".gitignore", b"ignored.txt\n");
    rig.write("ignored.txt", b"also retained");
    let captured = rig.capture("capture-1", &[]);
    rig.write("notes.txt", b"changed");
    fs::remove_dir_all(&rig.source).unwrap();
    let store = ArtifactStore::open(&rig.root, limits(), git_config()).unwrap();
    let prepared = store
        .materialize_candidate(&captured.snapshot, &rig.destination("repair"))
        .unwrap();
    assert_eq!(
        fs::read(prepared.destination.join("notes.txt")).unwrap(),
        b"retained untracked work\0\xff"
    );
    assert_eq!(
        fs::read(prepared.destination.join("ignored.txt")).unwrap(),
        b"also retained"
    );
    assert!(!prepared.destination.join(".git").exists());
    assert_eq!(
        store.open_snapshot(&captured.snapshot).unwrap().id(),
        &captured.snapshot
    );
}

#[test]
fn slots_preserve_raw_bytes_hash_and_executable_metadata() {
    let rig = Rig::new();
    rig.write("src/run", b"#!/bin/sh\r\nprintf raw\n\0\xff");
    rig.write("notes.txt", b"repair-only notes");
    fs::set_permissions(
        rig.source.join("src/run"),
        fs::Permissions::from_mode(0o4755),
    )
    .unwrap();
    fs::create_dir(rig.source.join("empty")).unwrap();
    let captured = rig.capture("code", &[slot("code", &["src", "empty"])]);
    let prepared = rig
        .store
        .materialize(
            &[SlotBinding {
                snapshot: captured.snapshot.clone(),
                output: "code".into(),
                into: "inputs/b".into(),
            }],
            &rig.destination("consumer"),
        )
        .unwrap();
    let actual = prepared.destination.join("inputs/b/src/run");
    assert_eq!(
        fs::read(&actual).unwrap(),
        b"#!/bin/sh\r\nprintf raw\n\0\xff"
    );
    assert_eq!(
        fs::metadata(&actual).unwrap().permissions().mode() & 0o7777,
        0o700
    );
    assert!(prepared.destination.join("inputs/b/empty").is_dir());
    assert!(!prepared.destination.join("inputs/b/notes.txt").exists());
    let snapshot = rig.store.open_snapshot(&captured.snapshot).unwrap();
    let original = snapshot
        .manifest()
        .entries
        .iter()
        .find(|e| e.path() == "src/run")
        .unwrap();
    let materialized = prepared
        .entries
        .iter()
        .find(|e| e.path() == "inputs/b/src/run")
        .unwrap();
    match (original, materialized) {
        (
            Entry::File {
                digest: a,
                bytes: x,
                executable: p,
                ..
            },
            Entry::File {
                digest: b,
                bytes: y,
                executable: q,
                ..
            },
        ) => assert_eq!((a, x, p), (b, y, q)),
        _ => panic!("expected exact file metadata"),
    }
}

#[test]
fn capture_replay_never_reselects_changed_or_deleted_source() {
    let rig = Rig::new();
    rig.write("notes.txt", b"original");
    let outputs = [slot("code", &["notes.txt"])];
    let original = rig.capture("operation-1", &outputs);
    rig.write("notes.txt", b"different");
    let replay = rig.capture("operation-1", &outputs);
    assert!(replay.replayed);
    assert_eq!(replay.snapshot, original.snapshot);
    let changed = rig.capture("operation-2", &outputs);
    assert_ne!(changed.snapshot, original.snapshot);
    fs::remove_dir_all(&rig.source).unwrap();
    assert_eq!(
        rig.capture("operation-1", &outputs).snapshot,
        original.snapshot
    );
    let result = rig.store.capture(CaptureRequest {
        key: "operation-1",
        source: QuiescentSource {
            root: &rig.source,
            boundary_id: "another-boundary",
        },
        outputs: &outputs,
    });
    assert!(matches!(result, Err(Error::Conflict)));
    let result = rig.store.capture(CaptureRequest {
        key: "operation-1",
        source: QuiescentSource {
            root: &rig.source,
            boundary_id: "test-stopped-runtime-1",
        },
        outputs: &[slot("other", &["notes.txt"])],
    });
    assert!(matches!(result, Err(Error::Conflict)));
}

#[test]
fn content_identity_is_deterministic_across_sources_keys_and_slot_order() {
    let a = Rig::new();
    let b = Rig::new();
    for rig in [&a, &b] {
        rig.write("a", b"same");
        rig.write("dir/b", b"other");
    }
    let one = a.capture("one", &[slot("z", &["dir/b"]), slot("a", &["a"])]);
    let two = b.capture("two", &[slot("a", &["a"]), slot("z", &["dir/b"])]);
    assert_eq!(one.snapshot, two.snapshot);
    let again = a.capture("another", &[slot("a", &["a"]), slot("z", &["dir/b"])]);
    assert_eq!(again.snapshot, one.snapshot);
    assert_eq!(fs::read_dir(a.root.join("snapshots")).unwrap().count(), 1);
    fs::set_permissions(b.source.join("a"), fs::Permissions::from_mode(0o700)).unwrap();
    assert_ne!(
        one.snapshot,
        b.capture("mode", &[slot("a", &["a"]), slot("z", &["dir/b"])])
            .snapshot
    );
}

#[test]
fn same_key_concurrent_capture_has_one_receipt_and_one_snapshot() {
    let rig = Arc::new(Rig::new());
    rig.write("notes.txt", b"concurrent");
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let rig = Arc::clone(&rig);
            std::thread::spawn(move || rig.capture("same-key", &[slot("notes", &["notes.txt"])]))
        })
        .collect();
    let receipts: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(receipts.iter().filter(|r| !r.replayed).count(), 1);
    assert!(receipts.iter().all(|r| r.snapshot == receipts[0].snapshot));
    assert_eq!(fs::read_dir(rig.root.join("captures")).unwrap().count(), 1);
}

#[test]
fn missing_output_is_explicit_but_failed_candidate_remains_repairable() {
    let rig = Rig::new();
    rig.write("notes.txt", b"useful partial work");
    let captured = rig.capture("failed", &[slot("code", &["not-created", "notes.txt"])]);
    assert_eq!(captured.missing_outputs[0].paths, ["not-created"]);
    let result = rig.store.materialize(
        &[SlotBinding {
            snapshot: captured.snapshot.clone(),
            output: "code".into(),
            into: "input".into(),
        }],
        &rig.destination("ordinary"),
    );
    assert!(matches!(result, Err(Error::MissingOutput { .. })));
    assert!(!rig.destination("ordinary").exists());
    rig.store
        .materialize_candidate(&captured.snapshot, &rig.destination("repair"))
        .unwrap();
    assert_eq!(
        fs::read(rig.destination("repair/notes.txt")).unwrap(),
        b"useful partial work"
    );
}

fn first_object(rig: &Rig, id: &Digest) -> std::path::PathBuf {
    let snapshot = rig.store.open_snapshot(id).unwrap();
    let digest = snapshot
        .manifest()
        .entries
        .iter()
        .find_map(|e| match e {
            Entry::File { digest, .. } => Some(digest),
            _ => None,
        })
        .unwrap();
    rig.root
        .join("snapshots")
        .join(id.as_str())
        .join("objects")
        .join(digest.as_str())
}

#[test]
fn content_identity_rejects_same_length_corruption_before_materialization() {
    let rig = Rig::new();
    rig.write("notes.txt", b"correct");
    let captured = rig.capture("one", &[slot("code", &["notes.txt"])]);
    fs::write(first_object(&rig, &captured.snapshot), b"corrupt").unwrap();
    assert!(matches!(
        rig.store.open_snapshot(&captured.snapshot),
        Err(Error::Integrity(_))
    ));
    assert!(
        rig.store
            .materialize_candidate(&captured.snapshot, &rig.destination("repair"))
            .is_err()
    );
    assert!(
        rig.store
            .materialize(
                &[SlotBinding {
                    snapshot: captured.snapshot,
                    output: "code".into(),
                    into: "input".into()
                }],
                &rig.destination("ordinary")
            )
            .is_err()
    );
    assert!(!rig.destination("repair").exists());
    assert!(!rig.destination("ordinary").exists());
}

#[test]
fn missing_object_and_manifest_corruption_fail_closed() {
    let rig = Rig::new();
    rig.write("notes.txt", b"content");
    let captured = rig.capture("one", &[]);
    let object = first_object(&rig, &captured.snapshot);
    fs::remove_file(&object).unwrap();
    assert!(
        rig.store
            .materialize_candidate(&captured.snapshot, &rig.destination("repair"))
            .is_err()
    );
    assert!(!rig.destination("repair").exists());
    fs::write(object, b"content").unwrap();
    let manifest = rig
        .root
        .join("snapshots")
        .join(captured.snapshot.as_str())
        .join("manifest.json");
    let mut bytes = fs::read(&manifest).unwrap();
    bytes.push(b' ');
    fs::write(manifest, bytes).unwrap();
    assert!(matches!(
        rig.store.open_snapshot(&captured.snapshot),
        Err(Error::Integrity(_))
    ));
}

#[test]
fn store_object_symlink_is_rejected_even_when_internal_and_byte_identical() {
    let rig = Rig::new();
    rig.write("notes.txt", b"content");
    let captured = rig.capture("one", &[]);
    let object = first_object(&rig, &captured.snapshot);
    fs::rename(&object, object.with_file_name("alternate")).unwrap();
    symlink("alternate", &object).unwrap();
    assert!(rig.store.open_snapshot(&captured.snapshot).is_err());
}

#[test]
fn source_symlink_is_rejected_without_following_internal_or_external_targets() {
    for internal in [true, false] {
        let rig = Rig::new();
        rig.write("notes.txt", b"ordinary");
        fs::write(rig.destination("outside"), b"must not be captured").unwrap();
        symlink(
            if internal { "notes.txt" } else { "../outside" },
            rig.source.join("link"),
        )
        .unwrap();
        assert!(
            rig.store
                .capture(CaptureRequest {
                    key: "link",
                    source: QuiescentSource {
                        root: &rig.source,
                        boundary_id: "stopped"
                    },
                    outputs: &[]
                })
                .is_err()
        );
        assert_eq!(fs::read_dir(rig.root.join("snapshots")).unwrap().count(), 0);
    }
}

#[test]
fn tracked_submodule_or_symlink_is_rejected_even_without_working_tree_marker() {
    for mode in ["160000", "120000"] {
        let rig = Rig::new();
        rig.write("ordinary", b"plain");
        // update-index writes a real index entry without requiring an initialized
        // submodule, working-tree symlink, .gitmodules or network operations.
        git(
            &rig.source,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("{mode},1111111111111111111111111111111111111111,ordinary"),
            ],
        );
        assert!(matches!(
            rig.store.capture(CaptureRequest {
                key: mode,
                source: QuiescentSource {
                    root: &rig.source,
                    boundary_id: "stopped"
                },
                outputs: &[]
            }),
            Err(Error::Unsupported(_))
        ));
    }
}

#[test]
fn nested_git_markers_gitmodules_special_files_and_hardlinks_are_rejected() {
    for case in ["nested", "gitmodules", "fifo", "hardlink"] {
        let rig = Rig::new();
        rig.write("notes.txt", b"plain");
        match case {
            "nested" => {
                fs::create_dir_all(rig.source.join("nested/.git")).unwrap();
            }
            "gitmodules" => rig.write(".gitmodules", b"[submodule]\n"),
            "fifo" => nix::unistd::mkfifo(&rig.source.join("fifo"), nix::sys::stat::Mode::S_IRUSR)
                .unwrap(),
            "hardlink" => {
                fs::hard_link(rig.source.join("notes.txt"), rig.source.join("alias")).unwrap()
            }
            _ => unreachable!(),
        }
        assert!(
            rig.store
                .capture(CaptureRequest {
                    key: case,
                    source: QuiescentSource {
                        root: &rig.source,
                        boundary_id: "stopped"
                    },
                    outputs: &[]
                })
                .is_err(),
            "{case}"
        );
    }
}

#[test]
fn slot_paths_reject_traversal_absolute_reserved_and_overlapping_names() {
    for paths in [
        vec!["../outside"],
        vec!["/outside"],
        vec!["a/../outside"],
        vec!["a//b"],
        vec!["a\\b"],
        vec![".git"],
        vec![".gitmodules"],
        vec!["a", "a"],
        vec!["a", "a-", "a/b"],
        vec!["a/"],
    ] {
        let rig = Rig::new();
        rig.write("notes.txt", b"plain");
        assert!(
            rig.store
                .capture(CaptureRequest {
                    key: "invalid",
                    source: QuiescentSource {
                        root: &rig.source,
                        boundary_id: "stopped"
                    },
                    outputs: &[slot("code", &paths)]
                })
                .is_err(),
            "{paths:?}"
        );
    }
    let rig = Rig::new();
    for outputs in [
        vec![slot("a", &["a"]), slot("b", &["a/b"])],
        vec![slot("a", &["a"]), slot("a", &["b"])],
        vec![slot("a", &[])],
    ] {
        assert!(
            rig.store
                .capture(CaptureRequest {
                    key: "invalid",
                    source: QuiescentSource {
                        root: &rig.source,
                        boundary_id: "stopped"
                    },
                    outputs: &outputs
                })
                .is_err()
        );
    }
}

#[test]
fn multiple_inputs_are_disjoint_and_existing_destination_is_never_overwritten() {
    let rig = Rig::new();
    rig.write("value", b"21");
    let captured = rig.capture("a", &[slot("value", &["value"])]);
    let bind = |into: &str| SlotBinding {
        snapshot: captured.snapshot.clone(),
        output: "value".into(),
        into: into.into(),
    };
    rig.store
        .materialize(
            &[bind("inputs/a"), bind("inputs/b")],
            &rig.destination("consumer"),
        )
        .unwrap();
    assert_eq!(
        fs::read(rig.destination("consumer/inputs/a/value")).unwrap(),
        b"21"
    );
    assert_eq!(
        fs::read(rig.destination("consumer/inputs/b/value")).unwrap(),
        b"21"
    );
    for bindings in [
        vec![bind("a"), bind("a/b")],
        vec![bind("a"), bind("a")],
        vec![bind("../escape")],
    ] {
        assert!(
            rig.store
                .materialize(&bindings, &rig.destination("invalid"))
                .is_err()
        );
        assert!(!rig.destination("invalid").exists());
    }
    fs::create_dir(rig.destination("existing")).unwrap();
    assert!(matches!(
        rig.store
            .materialize(&[bind("a")], &rig.destination("existing")),
        Err(Error::DestinationExists(_))
    ));
    assert_eq!(
        fs::read_dir(rig.destination("existing")).unwrap().count(),
        0
    );
    symlink(rig.destination("existing"), rig.destination("link")).unwrap();
    assert!(
        rig.store
            .materialize(&[bind("a")], &rig.destination("link"))
            .is_err()
    );
}

#[test]
fn raw_bytes_ignore_git_filters_attributes_and_fsmonitor_hooks() {
    let rig = Rig::new();
    rig.write(".gitattributes", b"payload filter=evil text eol=lf\n");
    rig.write("payload", b"raw\r\n\0\xff");
    let sentinel = rig.destination("hook-ran");
    let command = format!("touch {}; cat", sentinel.display());
    git(&rig.source, &["config", "filter.evil.clean", &command]);
    git(&rig.source, &["config", "filter.evil.smudge", &command]);
    git(&rig.source, &["config", "core.fsmonitor", &command]);
    let captured = rig.capture("raw", &[slot("raw", &["payload"])]);
    rig.store
        .materialize_candidate(&captured.snapshot, &rig.destination("restored"))
        .unwrap();
    assert_eq!(
        fs::read(rig.destination("restored/payload")).unwrap(),
        b"raw\r\n\0\xff"
    );
    assert!(!sentinel.exists());
}

#[test]
fn resource_limits_cover_capture_open_and_aggregate_materialization() {
    let rig = Rig::new();
    rig.write("value", b"four");
    for config in [
        {
            let mut x = limits();
            x.max_file_bytes = 3;
            x
        },
        {
            let mut x = limits();
            x.max_total_bytes = 3;
            x
        },
        {
            let mut x = limits();
            x.max_manifest_bytes = 8;
            x
        },
    ] {
        let store = ArtifactStore::open(&rig.root, config, git_config()).unwrap();
        assert!(matches!(
            store.capture(CaptureRequest {
                key: "over-limit",
                source: QuiescentSource {
                    root: &rig.source,
                    boundary_id: "stopped"
                },
                outputs: &[]
            }),
            Err(Error::Limit(_))
        ));
    }
    let captured = rig.capture("ok", &[slot("value", &["value"])]);
    let mut small = limits();
    small.max_total_bytes = 3;
    assert!(matches!(
        ArtifactStore::open(&rig.root, small, git_config())
            .unwrap()
            .open_snapshot(&captured.snapshot),
        Err(Error::Limit(_))
    ));
    let mut small = limits();
    small.max_total_bytes = 7;
    let store = ArtifactStore::open(&rig.root, small, git_config()).unwrap();
    let inputs: Vec<_> = ["a", "b"]
        .iter()
        .map(|into| SlotBinding {
            snapshot: captured.snapshot.clone(),
            output: "value".into(),
            into: (*into).into(),
        })
        .collect();
    assert!(matches!(
        store.materialize(&inputs, &rig.destination("overflow")),
        Err(Error::Limit(_))
    ));
    assert!(!rig.destination("overflow").exists());
    rig.write("deep/extra", b"");
    let mut small = limits();
    small.max_entries = 2;
    let store = ArtifactStore::open(&rig.root, small, git_config()).unwrap();
    assert!(matches!(
        store.capture(CaptureRequest {
            key: "entries",
            source: QuiescentSource {
                root: &rig.source,
                boundary_id: "stopped"
            },
            outputs: &[]
        }),
        Err(Error::Limit(_))
    ));
}

#[test]
fn digest_and_source_store_admission_reject_unsafe_roots() {
    for value in ["../escape", "A", &"a".repeat(63), &"A".repeat(64)] {
        assert!(value.parse::<Digest>().is_err());
    }
    let rig = Rig::new();
    let nested_store =
        ArtifactStore::open(&rig.source.join("store"), limits(), git_config()).unwrap();
    assert!(
        nested_store
            .capture(CaptureRequest {
                key: "overlap",
                source: QuiescentSource {
                    root: &rig.source,
                    boundary_id: "stopped"
                },
                outputs: &[]
            })
            .is_err()
    );
    symlink(&rig.root, rig.destination("store-link")).unwrap();
    assert!(ArtifactStore::open(&rig.destination("store-link"), limits(), git_config()).is_err());
    fs::create_dir(rig.destination("unrelated")).unwrap();
    fs::set_permissions(
        rig.destination("unrelated"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    fs::write(rig.destination("unrelated/valuable"), b"preserve").unwrap();
    assert!(ArtifactStore::open(&rig.destination("unrelated"), limits(), git_config()).is_err());
    assert_eq!(
        fs::read(rig.destination("unrelated/valuable")).unwrap(),
        b"preserve"
    );
}
