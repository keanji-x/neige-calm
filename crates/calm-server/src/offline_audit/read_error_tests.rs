use super::{PartialRead, TrustedReadRoot, inventory_owned_tree};
use crate::planner_attachments::dir::read_fault;
use std::path::Path;

#[test]
fn conversation_upgrade_fs_recovered_read_error_is_partial() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("nested")).unwrap();
    std::fs::write(dir.path().join("nested/raw"), b"real file bytes").unwrap();
    let root = TrustedReadRoot::open_absolute(dir.path()).unwrap();
    let _fault = read_fault::install([Some(nix::libc::EIO)]);
    let mut recovered = Vec::new();
    let mut callback_ok = false;
    let result = inventory_owned_tree(&root, |path, input| {
        assert_eq!(path, Path::new("nested/raw"));
        let error = input.read(&mut [0; 1]).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(nix::libc::EIO));
        input.read_to_end(&mut recovered)?;
        callback_ok = true;
        Ok(())
    });
    assert!(callback_ok);
    assert_eq!(recovered, b"real file bytes");
    let PartialRead { relative, error } = result.unwrap_err();
    assert_eq!(relative, Path::new("nested/raw"));
    assert_eq!(error.raw_os_error(), Some(nix::libc::EIO));
}

#[test]
fn conversation_upgrade_fs_read_error_first_cause_survives_later_errors() {
    for callback_fails in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("raw"), b"data").unwrap();
        let root = TrustedReadRoot::open_absolute(dir.path()).unwrap();
        let _fault = read_fault::install([Some(nix::libc::EIO), Some(nix::libc::EACCES)]);
        let mut recovered = Vec::new();
        let error = inventory_owned_tree(&root, |_, input| {
            for errno in [nix::libc::EIO, nix::libc::EACCES] {
                assert_eq!(
                    input.read(&mut [0; 1]).unwrap_err().raw_os_error(),
                    Some(errno)
                );
            }
            input.read_to_end(&mut recovered)?;
            if callback_fails {
                Err(std::io::ErrorKind::BrokenPipe.into())
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(recovered, b"data");
        assert_eq!(error.relative, Path::new("raw"));
        assert_eq!(error.error.raw_os_error(), Some(nix::libc::EIO));
    }
}

#[test]
fn conversation_upgrade_fs_interrupted_read_retry_is_partial() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("raw"), b"data").unwrap();
    let root = TrustedReadRoot::open_absolute(dir.path()).unwrap();
    let _fault = read_fault::install([Some(nix::libc::EINTR)]);
    let mut recovered = Vec::new();
    let mut callback_ok = false;
    let error = inventory_owned_tree(&root, |_, input| {
        // Read::read_to_end retries Interrupted itself; the reader still observed it.
        input.read_to_end(&mut recovered)?;
        callback_ok = true;
        Ok(())
    })
    .unwrap_err();
    assert!(callback_ok);
    assert_eq!(recovered, b"data");
    assert_eq!(error.relative, Path::new("raw"));
    assert_eq!(error.error.raw_os_error(), Some(nix::libc::EINTR));
}

#[test]
fn conversation_upgrade_fs_errors_after_bytes_or_eof_are_partial() {
    for contents in [b"".as_slice(), b"data".as_slice()] {
        for after_eof in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("raw"), contents).unwrap();
            let root = TrustedReadRoot::open_absolute(dir.path()).unwrap();
            let mut recovered = Vec::new();
            let mut observed_error = false;
            let mut callback_ok = false;
            let error = inventory_owned_tree(&root, |_, input| {
                if after_eof {
                    input.read_to_end(&mut recovered)?;
                } else {
                    recovered.resize(contents.len(), 0);
                    input.read_exact(&mut recovered)?;
                }
                let _fault = read_fault::install([Some(nix::libc::EIO)]);
                assert_eq!(
                    input.read(&mut [0; 1]).unwrap_err().raw_os_error(),
                    Some(nix::libc::EIO)
                );
                observed_error = true;
                assert_eq!(input.read(&mut [0; 1])?, 0);
                callback_ok = true;
                Ok(())
            })
            .unwrap_err();
            assert!(observed_error && callback_ok);
            assert_eq!(recovered, contents);
            assert_eq!(error.relative, Path::new("raw"));
            assert_eq!(error.error.raw_os_error(), Some(nix::libc::EIO));
        }
    }
}

#[test]
fn conversation_upgrade_fs_final_eof_probe_error_is_partial() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("raw"), b"data").unwrap();
    let root = TrustedReadRoot::open_absolute(dir.path()).unwrap();
    // The callback consumes exactly four bytes; the reader's EOF probe then fails.
    let _fault = read_fault::install([None, Some(nix::libc::EIO)]);
    let mut recovered = [0; 4];
    let error =
        inventory_owned_tree(&root, |_, input| input.read_exact(&mut recovered)).unwrap_err();
    assert_eq!(&recovered, b"data");
    assert_eq!(error.relative, Path::new("raw"));
    assert_eq!(error.error.raw_os_error(), Some(nix::libc::EIO));
}
