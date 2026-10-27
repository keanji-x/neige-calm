use crate::offline_audit::{TrustedReadRoot, inventory_owned_tree};
use std::path::Path;

#[test]
fn conversation_upgrade_fs_secure_regular_core_refuses_special_files_and_traversal() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("regular"), b"regular").unwrap();
    std::fs::create_dir(dir.path().join("directory")).unwrap();
    std::os::unix::fs::symlink("regular", dir.path().join("link")).unwrap();
    nix::unistd::mkfifo(&dir.path().join("fifo"), nix::sys::stat::Mode::S_IRUSR).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(dir.path().join("socket")).unwrap();
    let root = TrustedReadRoot::open_absolute(dir.path()).unwrap();
    for path in [
        "directory",
        "link",
        "fifo",
        "socket",
        "../outside",
        "/etc/passwd",
        "missing",
    ] {
        assert!(root.regular(Path::new(path)).is_err(), "{path}");
    }
    assert!(TrustedReadRoot::open_absolute(&dir.path().join("regular")).is_err());
    assert!(TrustedReadRoot::open_absolute(Path::new("relative")).is_err());
    assert_eq!(
        root.regular(Path::new("regular"))
            .unwrap()
            .metadata()
            .unwrap()
            .len(),
        7
    );
}

#[test]
fn conversation_upgrade_fs_roots_refuse_symlinks_and_missing_components() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("source/inner")).unwrap();
    std::os::unix::fs::symlink("source", dir.path().join("link")).unwrap();
    for path in ["link", "link/inner", "missing", "source/../source"] {
        assert!(
            TrustedReadRoot::open_absolute(&dir.path().join(path)).is_err(),
            "{path}"
        );
    }
    assert!(TrustedReadRoot::open_absolute(&dir.path().join("source/inner")).is_ok());
}

#[test]
fn conversation_upgrade_fs_renamed_root_retains_original_read_authority() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("raw"), b"original").unwrap();
    let root = TrustedReadRoot::open_absolute(&source).unwrap();
    std::fs::rename(&source, dir.path().join("moved")).unwrap();
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("raw"), b"replacement").unwrap();
    let mut bytes = Vec::new();
    let entries = inventory_owned_tree(&root, |_, input| {
        input.read_to_end(&mut bytes)?;
        Ok(())
    })
    .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].relative, Path::new("raw"));
    assert_eq!(bytes, b"original");
    assert_eq!(std::fs::read(source.join("raw")).unwrap(), b"replacement");
}

#[test]
fn conversation_upgrade_fs_root_and_regular_descriptors_are_readonly() {
    use std::{io::Write, os::fd::AsRawFd};
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("raw"), b"original").unwrap();
    let root = TrustedReadRoot::open_absolute(dir.path()).unwrap();
    let flags = nix::fcntl::fcntl(root.0.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFL).unwrap();
    assert_eq!(flags & nix::libc::O_ACCMODE, nix::libc::O_RDONLY);
    let mut file = root.regular(Path::new("raw")).unwrap();
    let error = file.write_all(b"replacement").unwrap_err();
    assert_eq!(error.raw_os_error(), Some(nix::libc::EBADF));
    assert_eq!(std::fs::read(dir.path().join("raw")).unwrap(), b"original");
}

#[test]
fn conversation_upgrade_fs_non_utf8_root_path_is_preserved() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join(std::ffi::OsStr::from_bytes(b"root-\xff"));
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("raw"), b"data").unwrap();
    let root = TrustedReadRoot::open_absolute(&source).unwrap();
    let mut bytes = Vec::new();
    let entries = inventory_owned_tree(&root, |_, input| {
        input.read_to_end(&mut bytes)?;
        Ok(())
    })
    .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(bytes, b"data");
}
