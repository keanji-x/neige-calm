use super::*;
use crate::offline_audit::BuildError;
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
fn conversation_upgrade_fs_replaced_temporary_root_cannot_publish() {
    let dir = tempfile::tempdir().unwrap();
    let output = super::tests::publisher(dir.path(), "final");
    let result = output.build(|writer| {
        writer.write_file(Path::new("raw"), &mut &b"expected"[..])?;
        let temporary = fs::read_dir(dir.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::rename(&temporary, dir.path().join("detached")).unwrap();
        fs::create_dir(&temporary).unwrap();
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(temporary.join("raw"), b"replacement").unwrap();
        Ok::<_, PublicationError>(())
    });
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
    assert!(!dir.path().join("final").exists());
}

#[test]
fn conversation_upgrade_fs_inventory_eacces_consumes_completion() {
    // SAFETY: geteuid has no arguments and does not access caller memory.
    // This regression requires the real non-root permission boundary.
    assert_ne!(unsafe { nix::libc::geteuid() }, 0);
    let source = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    fs::write(source.path().join("a"), b"copied first").unwrap();
    fs::write(source.path().join("z"), b"unreadable").unwrap();
    fs::set_permissions(source.path().join("z"), fs::Permissions::from_mode(0o0)).unwrap();
    let root = TrustedReadRoot::open_absolute(source.path()).unwrap();
    let output = super::tests::publisher(destination.path(), "final");
    let result = output.build(|writer| {
        writer.inventory_owned_tree(&root, |writer, path, input| {
            writer
                .write_file(path, input)
                .map_err(|e| io::Error::other(format!("{e:?}")))
        })
    });
    fs::set_permissions(source.path().join("z"), fs::Permissions::from_mode(0o600)).unwrap();
    let Err(BuildError::Build(partial)) = result else {
        panic!("{result:?}")
    };
    assert_eq!(partial.relative, Path::new("z"));
    assert_eq!(partial.error.kind(), io::ErrorKind::PermissionDenied);
    assert!(!destination.path().join("final").exists());
    let temporary = fs::read_dir(destination.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(fs::read(temporary.join("a")).unwrap(), b"copied first");
}

#[test]
fn conversation_upgrade_fs_build_checks_actual_inventory_source_overlap() {
    let source = tempfile::tempdir().unwrap();
    let root = TrustedReadRoot::open_absolute(source.path()).unwrap();
    let output = super::tests::publisher(source.path(), "final");
    let result = output.build(|writer| writer.inventory_owned_tree(&root, |_, _, _| Ok(())));
    let Err(BuildError::Build(partial)) = result else {
        panic!("{result:?}")
    };
    assert_eq!(partial.error.kind(), io::ErrorKind::InvalidInput);
    assert!(!source.path().join("final").exists());
}
