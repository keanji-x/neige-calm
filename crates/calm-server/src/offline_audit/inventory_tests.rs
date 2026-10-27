#[cfg(all(test, target_os = "linux"))]
mod conversation_upgrade_fs {
    use crate::offline_audit::{Diagnostic, EntryKind, TrustedReadRoot, inventory_owned_tree};
    use sha2::{Digest, Sha256};
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    #[test]
    fn no_omission_raw_names_unknown_and_duplicate_locations() {
        let dir = tempfile::tempdir().unwrap();
        for location in ["unknown/bound", "unknown/staging"] {
            std::fs::create_dir_all(dir.path().join(location)).unwrap();
            std::fs::write(dir.path().join(location).join("same.part"), location).unwrap();
        }
        let raw = std::ffi::OsStr::from_bytes(b"orphan-\xff");
        std::fs::write(dir.path().join(raw), b"raw bytes").unwrap();
        let raw_directory = std::ffi::OsStr::from_bytes(b"directory-\xfe");
        std::fs::create_dir(dir.path().join(raw_directory)).unwrap();
        std::fs::write(dir.path().join(raw_directory).join("nested"), b"nested").unwrap();
        std::os::unix::fs::symlink("missing-target", dir.path().join("unknown-link")).unwrap();
        nix::unistd::mkfifo(
            &dir.path().join("unknown-fifo"),
            nix::sys::stat::Mode::S_IRUSR,
        )
        .unwrap();
        let _socket =
            std::os::unix::net::UnixListener::bind(dir.path().join("unknown-socket")).unwrap();
        let root = TrustedReadRoot::open_absolute(dir.path()).unwrap();
        let mut content = std::collections::BTreeMap::<_, Vec<u8>>::new();
        let entries = inventory_owned_tree(&root, |p, data| {
            data.read_to_end(content.entry(p.to_path_buf()).or_default())?;
            Ok(())
        })
        .unwrap();
        assert_eq!(entries.len(), 11);
        assert_eq!(content.len(), 4);
        assert_eq!(content[Path::new(raw)], b"raw bytes");
        assert_eq!(content[&Path::new(raw_directory).join("nested")], b"nested");
        assert!(
            entries
                .iter()
                .any(|e| e.relative == Path::new(raw_directory)
                    && e.kind == EntryKind::Directory
                    && e.diagnostics.contains(&Diagnostic::NonUtf8Name))
        );
        assert_ne!(
            content[Path::new("unknown/bound/same.part")],
            content[Path::new("unknown/staging/same.part")]
        );
        let file = entries
            .iter()
            .find(|e| e.relative == Path::new(raw))
            .unwrap();
        assert_eq!(file.kind, EntryKind::Regular);
        assert_eq!(file.sha256, Some(Sha256::digest(b"raw bytes").into()));
        assert!(file.diagnostics.contains(&Diagnostic::NonUtf8Name));
        for (name, kind) in [
            ("unknown-link", EntryKind::Symlink),
            ("unknown-fifo", EntryKind::Other),
            ("unknown-socket", EntryKind::Other),
        ] {
            let entry = entries
                .iter()
                .find(|e| e.relative == Path::new(name))
                .unwrap();
            assert_eq!(entry.kind, kind);
            assert!(entry.sha256.is_none());
            assert!(entry.diagnostics.contains(&Diagnostic::TargetNotRead));
        }
        assert!(
            entries
                .iter()
                .all(|e| e.diagnostics.contains(&Diagnostic::UninterpretedOwnership))
        );
    }

    #[test]
    fn read_sink_errors_are_partial() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file"), b"data").unwrap();
        let root = TrustedReadRoot::open_absolute(dir.path()).unwrap();
        let error = inventory_owned_tree(&root, |_, _| Err(io::ErrorKind::PermissionDenied.into()))
            .unwrap_err();
        assert_eq!(error.relative, Path::new("file"));
        assert_eq!(error.error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn observed_source_changes_are_partial() {
        for change in [
            "size",
            "same-size",
            "inode",
            "disappear",
            "add",
            "remove-directory",
            "symlink",
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("a"), b"data").unwrap();
            std::fs::File::open(dir.path().join("a"))
                .unwrap()
                .set_modified(std::time::SystemTime::UNIX_EPOCH)
                .unwrap();
            std::fs::write(dir.path().join("z"), b"later").unwrap();
            std::fs::create_dir(dir.path().join("sub")).unwrap();
            let root = TrustedReadRoot::open_absolute(dir.path()).unwrap();
            let mut changed = false;
            let result = inventory_owned_tree(&root, |_, input| {
                std::io::copy(input, &mut std::io::sink())?;
                if !changed {
                    changed = true;
                    match change {
                        "size" => std::fs::write(dir.path().join("a"), b"longer")?,
                        "same-size" => std::fs::write(dir.path().join("a"), b"xxxx")?,
                        "inode" => {
                            std::fs::rename(dir.path().join("z"), dir.path().join("old"))?;
                            std::fs::write(dir.path().join("z"), b"later")?;
                        }
                        "disappear" => std::fs::remove_file(dir.path().join("z"))?,
                        "add" => std::fs::write(dir.path().join("new"), b"new")?,
                        "remove-directory" => std::fs::remove_dir(dir.path().join("sub"))?,
                        "symlink" => {
                            std::fs::remove_file(dir.path().join("z"))?;
                            std::os::unix::fs::symlink("a", dir.path().join("z"))?;
                        }
                        _ => unreachable!(),
                    }
                }
                Ok(())
            });
            assert!(result.is_err(), "{change}");
        }
    }

    #[test]
    fn empty_inventory_does_not_create_directories() {
        let dir = tempfile::tempdir().unwrap();
        let root = TrustedReadRoot::open_absolute(dir.path()).unwrap();
        assert!(
            inventory_owned_tree(&root, |_, _| panic!("no bytes"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        assert!(TrustedReadRoot::open_absolute(&dir.path().join("missing")).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}

#[test]
fn conversation_upgrade_fs_permission_failures_are_partial() {
    use crate::offline_audit::{TrustedReadRoot, inventory_owned_tree};
    use std::os::unix::fs::PermissionsExt;
    // A privileged process bypasses the OS permission boundary exercised here.
    // SAFETY: geteuid has no arguments and does not access caller memory.
    if unsafe { nix::libc::geteuid() } == 0 {
        return;
    }
    for directory in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unreadable");
        if directory {
            std::fs::create_dir(&path).unwrap();
        } else {
            std::fs::write(&path, b"secret").unwrap();
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o0)).unwrap();
        let root = TrustedReadRoot::open_absolute(dir.path()).unwrap();
        let error = inventory_owned_tree(&root, |_, _| panic!("unreadable bytes")).unwrap_err();
        assert_eq!(error.error.kind(), std::io::ErrorKind::PermissionDenied);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

#[test]
fn conversation_upgrade_fs_repeated_streaming_leaves_frozen_source_unchanged() {
    use crate::offline_audit::{TrustedReadRoot, inventory_owned_tree};
    use std::os::unix::fs::MetadataExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("raw"), vec![23u8; 150000]).unwrap();
    let before = std::fs::metadata(dir.path().join("raw")).unwrap();
    let root = TrustedReadRoot::open_absolute(dir.path()).unwrap();
    for _ in 0..2 {
        let mut bytes = Vec::new();
        let entries = inventory_owned_tree(&root, |_, input| {
            input.read_to_end(&mut bytes)?;
            Ok(())
        })
        .unwrap();
        assert_eq!(bytes, vec![23u8; 150000]);
        assert_eq!(entries.len(), 1);
    }
    let after = std::fs::metadata(dir.path().join("raw")).unwrap();
    assert_eq!(
        (
            before.ino(),
            before.len(),
            before.mtime(),
            before.mtime_nsec(),
            before.ctime(),
            before.ctime_nsec()
        ),
        (
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec()
        )
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn conversation_upgrade_fs_mutation_during_fd_read_is_partial() {
    use crate::offline_audit::{TrustedReadRoot, inventory_owned_tree};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw");
    std::fs::write(&path, b"original").unwrap();
    let root = TrustedReadRoot::open_absolute(dir.path()).unwrap();
    let result = inventory_owned_tree(&root, |_, input| {
        input.read_exact(&mut [0u8; 1])?;
        std::fs::write(&path, b"changed-and-longer")?;
        std::io::copy(input, &mut std::io::sink())?;
        Ok(())
    });
    assert!(result.is_err());
}

#[test]
fn conversation_upgrade_fs_empty_regular_file_is_streamed_and_hashed() {
    use crate::offline_audit::{EntryKind, TrustedReadRoot, inventory_owned_tree};
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("empty"), b"").unwrap();
    let root = TrustedReadRoot::open_absolute(dir.path()).unwrap();
    let mut streamed = Vec::new();
    let entries = inventory_owned_tree(&root, |path, input| {
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes)?;
        streamed.push((path.to_path_buf(), bytes));
        Ok(())
    })
    .unwrap();
    assert_eq!(streamed, vec![("empty".into(), vec![])]);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, EntryKind::Regular);
    assert_eq!(entries[0].identity.size, 0);
    assert_eq!(entries[0].sha256, Some(Sha256::digest(b"").into()));
}

#[test]
fn conversation_upgrade_fs_unconsumed_stream_is_partial() {
    use crate::offline_audit::{TrustedReadRoot, inventory_owned_tree};
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("raw"), b"data").unwrap();
    let root = TrustedReadRoot::open_absolute(dir.path()).unwrap();
    for consumed in [0, 1, 3] {
        let error = inventory_owned_tree(&root, |_, input| {
            input.read_exact(&mut vec![0; consumed])?;
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.relative, std::path::Path::new("raw"));
    }
}
