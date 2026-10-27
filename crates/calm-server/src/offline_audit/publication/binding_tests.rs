use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn change(tree: &Path, kind: &str) {
    let relative = match kind {
        "root" => "",
        "directory" => "a",
        _ => "a/b/raw",
    };
    let path = tree.join(relative);
    match kind {
        "extra" => fs::write(tree.join("unexpected"), b"unsynced").unwrap(),
        "remove" => fs::remove_file(path).unwrap(),
        "rewrite" => {
            fs::write(&path, b"tampered").unwrap();
            File::open(path)
                .unwrap()
                .set_modified(std::time::SystemTime::UNIX_EPOCH)
                .unwrap();
        }
        "symlink" => {
            fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink("../../manifest", path).unwrap();
        }
        _ => {
            fs::rename(&path, tree.parent().unwrap().join("detached")).unwrap();
            if kind == "file" {
                fs::write(&path, b"unsynced").unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
            } else {
                fs::create_dir(&path).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
    }
}

#[test]
fn conversation_upgrade_fs_all_output_bindings_and_contents_checked() {
    for kind in [
        "root",
        "directory",
        "file",
        "extra",
        "remove",
        "rewrite",
        "symlink",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut output = super::tests::publisher(dir.path(), "final");
        super::tests::fill(&mut output).unwrap();
        change(&dir.path().join(&output.temporary), kind);
        assert!(
            matches!(
                output.finish(),
                Err(PublicationError::Partial {
                    stage: Stage::ValidateBindings,
                    ..
                })
            ),
            "{kind}"
        );
        assert!(!dir.path().join("final").exists(), "{kind}");
    }
}

struct RaceIo {
    parent: PathBuf,
    kind: &'static str,
    window: &'static str,
}
impl Io for RaceIo {
    fn rename(&mut self, parent: &File, from: &OsStr, to: &OsStr) -> io::Result<()> {
        if self.window == "before-rename" {
            change(&self.parent.join(from), self.kind);
        }
        SystemIo.rename(parent, from, to)?;
        if self.window == "after-rename" {
            change(&self.parent.join(to), self.kind);
        }
        Ok(())
    }
    fn sync(&mut self, file: &File, stage: &Stage) -> io::Result<()> {
        if *stage == Stage::ParentSync && self.window == "parent-sync" {
            change(&self.parent.join("final"), self.kind);
        }
        file.sync_all()
    }
}

#[test]
fn conversation_upgrade_fs_rename_windows_never_confirm_replacements() {
    for window in ["before-rename", "after-rename", "parent-sync"] {
        for kind in [
            "root",
            "directory",
            "file",
            "extra",
            "remove",
            "rewrite",
            "symlink",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut output = super::tests::publisher(dir.path(), "final");
            super::tests::fill(&mut output).unwrap();
            output.io = Box::new(RaceIo {
                parent: dir.path().into(),
                kind,
                window,
            });
            assert!(
                matches!(
                    output.finish(),
                    Err(PublicationError::PublishedDurabilityUnconfirmed { .. })
                ),
                "{window}/{kind}"
            );
            assert!(dir.path().join("final").is_dir(), "{window}/{kind}");
            assert!(
                Publisher::new(
                    TrustedReadRoot::open_absolute(dir.path()).unwrap(),
                    OsStr::new("final"),
                    &[]
                )
                .is_err()
            );
        }
    }
}

#[test]
fn conversation_upgrade_fs_overlap_rechecked_after_root_moves() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("source")).unwrap();
    fs::create_dir(dir.path().join("output")).unwrap();
    let source = TrustedReadRoot::open_absolute(&dir.path().join("source")).unwrap();
    let output = Publisher::new(
        TrustedReadRoot::open_absolute(&dir.path().join("output")).unwrap(),
        OsStr::new("final"),
        &[&source],
    )
    .unwrap();
    fs::rename(dir.path().join("output"), dir.path().join("source/moved")).unwrap();
    assert!(output.finish().is_err());
    assert!(!dir.path().join("source/moved/final").exists());
}

struct WriteAfterSync(PathBuf);
impl Io for WriteAfterSync {
    fn sync(&mut self, file: &File, stage: &Stage) -> io::Result<()> {
        file.sync_all()?;
        if *stage == Stage::FileSync {
            fs::write(&self.0, b"tampered")?;
            File::open(&self.0)?.set_modified(std::time::SystemTime::UNIX_EPOCH)?;
        }
        Ok(())
    }
}
#[test]
fn conversation_upgrade_fs_file_metadata_cannot_change_during_sync() {
    let dir = tempfile::tempdir().unwrap();
    let mut output = super::tests::publisher(dir.path(), "final");
    output.io = Box::new(WriteAfterSync(
        dir.path().join(&output.temporary).join("raw"),
    ));
    assert!(matches!(
        output.write_file(Path::new("raw"), &mut &b"evidence"[..]),
        Err(PublicationError::Partial {
            stage: Stage::ValidateBindings,
            ..
        })
    ));
    assert!(matches!(
        output.finish(),
        Err(PublicationError::AlreadyFailed)
    ));
    assert!(!dir.path().join("final").exists());
}

#[test]
fn conversation_upgrade_fs_source_moved_into_output_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    fs::create_dir(base.join("source")).unwrap();
    fs::create_dir(base.join("output")).unwrap();
    let source = TrustedReadRoot::open_absolute(&base.join("source")).unwrap();
    let output = Publisher::new(
        TrustedReadRoot::open_absolute(&base.join("output")).unwrap(),
        OsStr::new("final"),
        &[&source],
    )
    .unwrap();
    let temporary = fs::read_dir(base.join("output"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::rename(base.join("source"), temporary.join("source")).unwrap();
    let result = output.build(|_| Ok::<(), ()>(()));
    assert!(
        matches!(
            result,
            Err(BuildError::Publication(PublicationError::Partial {
                stage: Stage::ValidateBindings,
                ..
            }))
        ),
        "{result:?}"
    );
    assert!(!base.join("output/final").exists());
}
#[test]
fn conversation_upgrade_fs_public_build_retains_source_and_parent_fds() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    fs::create_dir(base.join("source")).unwrap();
    fs::create_dir(base.join("output")).unwrap();
    fs::write(base.join("source/raw"), b"original").unwrap();
    let source = TrustedReadRoot::open_absolute(&base.join("source")).unwrap();
    let output = Publisher::new(
        TrustedReadRoot::open_absolute(&base.join("output")).unwrap(),
        OsStr::new("final"),
        &[&source],
    )
    .unwrap();
    fs::rename(base.join("source"), base.join("source-moved")).unwrap();
    fs::create_dir(base.join("source")).unwrap();
    fs::write(base.join("source/raw"), b"replacement").unwrap();
    fs::rename(base.join("output"), base.join("output-moved")).unwrap();
    fs::create_dir(base.join("output")).unwrap();
    let (entries, _) = output
        .build(|writer| {
            writer.inventory_owned_tree(&source, |writer, path, input| {
                writer
                    .write_file(path, input)
                    .map_err(|e| io::Error::other(format!("{e:?}")))
            })
        })
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].relative, Path::new("raw"));
    assert_eq!(
        fs::read(base.join("output-moved/final/raw")).unwrap(),
        b"original"
    );
    assert_eq!(
        fs::read(base.join("source-moved/raw")).unwrap(),
        b"original"
    );
    assert_eq!(fs::read(base.join("source/raw")).unwrap(), b"replacement");
    assert!(!base.join("output/final").exists());
}
