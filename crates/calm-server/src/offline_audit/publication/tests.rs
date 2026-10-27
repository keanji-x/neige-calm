use super::*;
use std::sync::{Arc, Mutex};

struct FaultIo {
    stage: Option<Stage>,
    remaining: usize,
    seen: Arc<Mutex<Vec<Stage>>>,
    unsupported: bool,
}
impl FaultIo {
    fn hits(&mut self, stage: &Stage) -> bool {
        self.seen.lock().unwrap().push(stage.clone());
        if self.stage.as_ref() != Some(stage) {
            return false;
        }
        self.remaining -= 1;
        self.remaining == 0
    }
}
impl Io for FaultIo {
    fn write(&mut self, file: &mut File, bytes: &[u8]) -> io::Result<()> {
        if self.hits(&Stage::Write) {
            // Exercise an actual failing write syscall, through the production IO door.
            File::open("/dev/null")?.write_all(bytes)
        } else {
            file.write_all(bytes)
        }
    }
    fn sync(&mut self, file: &File, stage: &Stage) -> io::Result<()> {
        if self.hits(stage) {
            File::open("/dev/null")?.sync_all()
        } else {
            file.sync_all()
        }
    }
    fn rename(&mut self, parent: &File, from: &OsStr, to: &OsStr) -> io::Result<()> {
        if self.hits(&Stage::Rename) {
            let (from, flags) = if self.unsupported {
                (from, RenameFlags::from_bits_retain(0x80000000))
            } else {
                (
                    OsStr::new("nonexistent-incomplete"),
                    RenameFlags::RENAME_NOREPLACE,
                )
            };
            renameat2(
                Some(parent.as_raw_fd()),
                from,
                Some(parent.as_raw_fd()),
                to,
                flags,
            )
            .map_err(io::Error::from)
        } else {
            SystemIo.rename(parent, from, to)
        }
    }
}
pub(super) fn publisher(path: &Path, destination: &str) -> Publisher {
    Publisher::new(
        TrustedReadRoot::open_absolute(path).unwrap(),
        OsStr::new(destination),
        &[],
    )
    .unwrap()
}
pub(super) fn fill(publisher: &mut Publisher) -> Result<(), PublicationError> {
    publisher.add_directory(Path::new("a"))?;
    publisher.add_directory(Path::new("a/b"))?;
    for path in ["a/b/raw", "manifest", "seal"] {
        publisher.write_file(Path::new(path), &mut &b"evidence"[..])?;
    }
    Ok(())
}

#[test]
fn conversation_upgrade_fs_durable_files_and_bottom_up_directories() {
    let dir = tempfile::tempdir().unwrap();
    let mut output = publisher(dir.path(), "final");
    let seen = Arc::new(Mutex::new(Vec::new()));
    output.io = Box::new(FaultIo {
        stage: None,
        remaining: 1,
        seen: seen.clone(),
        unsupported: false,
    });
    fill(&mut output).unwrap();
    output.finish().unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            Stage::Write,
            Stage::FileSync,
            Stage::Write,
            Stage::FileSync,
            Stage::Write,
            Stage::FileSync,
            Stage::DirectorySync("a/b".into()),
            Stage::DirectorySync("a".into()),
            Stage::DirectorySync("".into()),
            Stage::Rename,
            Stage::ParentSync,
        ]
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::read(dir.path().join("final/a/b/raw")).unwrap(),
        b"evidence"
    );
    assert_eq!(
        std::fs::metadata(dir.path().join("final"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(dir.path().join("final/seal"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn conversation_upgrade_fs_every_io_failure_is_non_success() {
    let mut faults = vec![
        (Stage::Write, 1),
        (Stage::Rename, 1),
        (Stage::ParentSync, 1),
    ];
    for occurrence in 1..=3 {
        faults.push((Stage::FileSync, occurrence));
    }
    for path in ["a/b", "a", ""] {
        faults.push((Stage::DirectorySync(path.into()), 1));
    }
    for (stage, occurrence) in faults {
        let dir = tempfile::tempdir().unwrap();
        let mut output = publisher(dir.path(), "final");
        let temporary = output.temporary.clone();
        output.io = Box::new(FaultIo {
            stage: Some(stage.clone()),
            remaining: occurrence,
            seen: Arc::default(),
            unsupported: false,
        });
        let error = match fill(&mut output) {
            Ok(()) => output.finish().unwrap_err(),
            Err(error) => {
                assert!(matches!(
                    output.finish(),
                    Err(PublicationError::AlreadyFailed)
                ));
                error
            }
        };
        match stage {
            Stage::ParentSync => {
                assert!(matches!(
                    error,
                    PublicationError::PublishedDurabilityUnconfirmed { .. }
                ));
                assert!(dir.path().join("final/seal").exists());
                assert!(
                    Publisher::new(
                        TrustedReadRoot::open_absolute(dir.path()).unwrap(),
                        OsStr::new("final"),
                        &[]
                    )
                    .is_err()
                );
            }
            Stage::Rename => assert!(matches!(error, PublicationError::PublishFailed { .. })),
            _ => assert!(
                matches!(error, PublicationError::Partial { stage: actual, .. } if actual == stage)
            ),
        }
        if stage != Stage::ParentSync {
            assert!(!dir.path().join("final").exists());
            assert!(dir.path().join(&temporary).is_dir());
            let retry = publisher(dir.path(), "final");
            assert_ne!(retry.temporary, temporary);
        }
    }
}

#[test]
fn conversation_upgrade_fs_no_clobber_two_contenders_and_existing_target() {
    let dir = tempfile::tempdir().unwrap();
    let mut first = publisher(dir.path(), "final");
    let mut second = publisher(dir.path(), "final");
    first
        .write_file(Path::new("data"), &mut &b"winner"[..])
        .unwrap();
    second
        .write_file(Path::new("data"), &mut &b"loser"[..])
        .unwrap();
    let loser = second.temporary.clone();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let wait = barrier.clone();
    let thread = std::thread::spawn(move || {
        wait.wait();
        first.finish()
    });
    barrier.wait();
    let second_result = second.finish();
    let first_result = thread.join().unwrap();
    assert_eq!(
        usize::from(first_result.is_ok()) + usize::from(second_result.is_ok()),
        1
    );
    let bytes = std::fs::read(dir.path().join("final/data")).unwrap();
    assert!(bytes == b"winner" || bytes == b"loser");
    if first_result.is_ok() {
        assert!(matches!(
            second_result,
            Err(PublicationError::PublishFailed { .. })
        ));
        assert!(dir.path().join(loser).is_dir());
    } else {
        assert!(matches!(
            first_result,
            Err(PublicationError::PublishFailed { .. })
        ));
    }
    assert!(
        Publisher::new(
            TrustedReadRoot::open_absolute(dir.path()).unwrap(),
            OsStr::new("final"),
            &[]
        )
        .is_err()
    );
    assert_eq!(std::fs::read(dir.path().join("final/data")).unwrap(), bytes);
}

#[test]
fn conversation_upgrade_fs_invalid_fresh_entries_and_read_failures() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["", ".", "..", "a/b", "/absolute"] {
        assert!(matches!(
            Publisher::new(
                TrustedReadRoot::open_absolute(dir.path()).unwrap(),
                OsStr::new(name),
                &[]
            ),
            Err(PublicationError::InvalidDestination)
        ));
    }
    for path in ["../escape", "/escape", "missing/file", ""] {
        let mut output = publisher(dir.path(), "final");
        assert!(matches!(
            output.write_file(Path::new(path), &mut &b"data"[..]),
            Err(PublicationError::Partial {
                stage: Stage::CreateFile,
                ..
            })
        ));
        assert!(matches!(
            output.finish(),
            Err(PublicationError::AlreadyFailed)
        ));
    }
    let mut output = publisher(dir.path(), "final");
    output.add_directory(Path::new("a")).unwrap();
    assert!(matches!(
        output.add_directory(Path::new("a")),
        Err(PublicationError::Partial {
            stage: Stage::CreateDirectory,
            ..
        })
    ));
    let mut output = publisher(dir.path(), "final");
    output
        .write_file(Path::new("file"), &mut &b"original"[..])
        .unwrap();
    assert!(matches!(
        output.write_file(Path::new("file"), &mut &b"replacement"[..]),
        Err(PublicationError::Partial {
            stage: Stage::CreateFile,
            ..
        })
    ));
    assert_eq!(
        std::fs::read(dir.path().join(&output.temporary).join("file")).unwrap(),
        b"original"
    );
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::UnexpectedEof.into())
        }
    }
    let mut output = publisher(dir.path(), "final");
    assert!(matches!(
        output.write_file(Path::new("raw"), &mut Broken),
        Err(PublicationError::Partial {
            stage: Stage::Read,
            ..
        })
    ));
}

#[test]
fn conversation_upgrade_fs_unsupported_rename_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let mut output = publisher(dir.path(), "final");
    let temporary = output.temporary.clone();
    output.io = Box::new(FaultIo {
        stage: Some(Stage::Rename),
        remaining: 1,
        seen: Arc::default(),
        unsupported: true,
    });
    fill(&mut output).unwrap();
    assert!(matches!(
        output.finish(),
        Err(PublicationError::Unsupported)
    ));
    assert!(!dir.path().join("final").exists());
    assert!(dir.path().join(temporary).exists());
}

#[test]
fn conversation_upgrade_fs_roots_symlinks_overlap_and_renamed_paths() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("source/inner")).unwrap();
    let source = TrustedReadRoot::open_absolute(&dir.path().join("source")).unwrap();
    let inner = TrustedReadRoot::open_absolute(&dir.path().join("source/inner")).unwrap();
    for parent in [source.directory(Path::new(".")).unwrap(), inner] {
        assert!(matches!(
            Publisher::new(parent, OsStr::new("out"), &[&source]),
            Err(PublicationError::Overlap)
        ));
    }
    // Reverse overlap: destination is the existing ancestor of the source.
    assert!(
        Publisher::new(
            TrustedReadRoot::open_absolute(dir.path()).unwrap(),
            OsStr::new("source"),
            &[&source]
        )
        .is_err()
    );
    std::fs::rename(dir.path().join("source"), dir.path().join("moved")).unwrap();
    std::fs::create_dir(dir.path().join("source")).unwrap();
    let parent = TrustedReadRoot::open_absolute(&dir.path().join("moved")).unwrap();
    assert!(matches!(
        Publisher::new(parent, OsStr::new("out"), &[&source]),
        Err(PublicationError::Overlap)
    ));
    // An unrelated fresh sibling under a common ancestor is permitted.
    Publisher::new(
        TrustedReadRoot::open_absolute(dir.path()).unwrap(),
        OsStr::new("fresh"),
        &[&source],
    )
    .unwrap()
    .finish()
    .unwrap();
}

#[test]
fn conversation_upgrade_fs_inventory_connects_directly_to_publisher_including_empty_files() {
    let source = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join("empty"), b"").unwrap();
    std::fs::write(source.path().join("raw"), vec![42u8; 150000]).unwrap();
    let root = TrustedReadRoot::open_absolute(source.path()).unwrap();
    let output = Publisher::new(
        TrustedReadRoot::open_absolute(destination.path()).unwrap(),
        OsStr::new("final"),
        &[&root],
    )
    .unwrap();
    let (entries, published) = output
        .build(|writer| {
            writer.add_directory(Path::new("caller")).unwrap();
            let entries = writer.inventory_owned_tree(&root, |writer, path, input| {
                writer
                    .write_file(path, input)
                    .map_err(|e| io::Error::other(format!("{e:?}")))
            })?;
            writer
                .write_file(Path::new("caller/additional"), &mut &b"owned"[..])
                .unwrap();
            Ok::<_, crate::offline_audit::PartialRead>(entries)
        })
        .unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(published, PublishedDurable);
    assert_eq!(
        std::fs::read(destination.path().join("final/caller/additional")).unwrap(),
        b"owned"
    );
    assert_eq!(
        std::fs::read(destination.path().join("final/empty")).unwrap(),
        b""
    );
    assert_eq!(
        std::fs::read(destination.path().join("final/raw")).unwrap(),
        vec![42u8; 150000]
    );
    assert!(crate::offline_audit::inventory_owned_tree(&root, |_, _| Ok(())).is_err());
}

#[test]
fn conversation_upgrade_fs_creation_permission_failure_preserves_non_success() {
    // SAFETY: geteuid has no arguments and does not access caller memory.
    if unsafe { nix::libc::geteuid() } == 0 {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = TrustedReadRoot::open_absolute(dir.path()).unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = Publisher::new(root, OsStr::new("final"), &[]);
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(
        result,
        Err(PublicationError::Partial {
            stage: Stage::CreateDirectory,
            ..
        })
    ));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
