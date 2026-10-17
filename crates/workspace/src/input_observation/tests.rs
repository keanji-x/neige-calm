use super::*;
use std::{
    fs,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt, fs::symlink},
};

fn frontier(path: &std::path::Path) -> InputLookupFrontier {
    InputLookupFrontier::from_directory(&File::open(path).unwrap()).unwrap()
}

fn lookup(parent: &InputLookupFrontier, name: &str) -> InputLookupObservation {
    let InputObservation::Lookup(observed) = parent
        .lookup_step(OsStr::new(name))
        .unwrap()
        .observe()
        .unwrap()
    else {
        panic!("lookup primitive returned a different operation");
    };
    observed
}

#[test]
fn input_observation_missing_name_is_actual_errno_and_not_a_permanent_negative() {
    let root = tempfile::tempdir().unwrap();
    let parent = frontier(root.path());
    let missing = lookup(&parent, "later");
    assert_eq!(missing.errno(), Some(libc::ENOENT));
    assert!(missing.file().is_none());
    missing.revalidate().unwrap();

    fs::write(root.path().join("later"), "now exists").unwrap();
    assert!(matches!(
        missing.revalidate(),
        Err(InputObservationError::Changed)
    ));
    assert_eq!(lookup(&parent, "later").kind(), Some(InputFileKind::File));
}

#[test]
fn input_observation_regular_file_prefix_reports_actual_enotdir() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file"), "not a directory").unwrap();
    let file = lookup(&frontier(root.path()), "file");
    let child = lookup(&file.next_frontier().unwrap(), "child");
    assert_eq!(child.errno(), Some(libc::ENOTDIR));
    child.revalidate().unwrap();
}

#[test]
fn input_observation_aliases_hold_the_actual_object_but_validate_each_original_name() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("first"), "original").unwrap();
    fs::hard_link(root.path().join("first"), root.path().join("alias")).unwrap();
    let parent = frontier(root.path());
    let first = lookup(&parent, "first");
    let alias = lookup(&parent, "alias");
    let original_inode = first.file().unwrap().metadata().unwrap().ino();
    assert_eq!(
        alias.file().unwrap().metadata().unwrap().ino(),
        original_inode
    );

    fs::rename(root.path().join("first"), root.path().join("old")).unwrap();
    fs::write(root.path().join("first"), "replacement").unwrap();
    assert!(matches!(
        first.revalidate(),
        Err(InputObservationError::Changed)
    ));
    alias.revalidate().unwrap();
    assert_eq!(
        first.file().unwrap().metadata().unwrap().ino(),
        original_inode
    );
    assert_ne!(
        fs::metadata(root.path().join("first")).unwrap().ino(),
        original_inode
    );
}

#[test]
fn input_observation_rename_of_intermediate_edge_invalidates_retained_descendant() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("inside")).unwrap();
    fs::write(root.path().join("inside/leaf"), "original").unwrap();
    let directory = lookup(&frontier(root.path()), "inside");
    let leaf = lookup(&directory.next_frontier().unwrap(), "leaf");

    fs::rename(root.path().join("inside"), root.path().join("moved")).unwrap();
    fs::create_dir(root.path().join("inside")).unwrap();
    fs::write(root.path().join("inside/leaf"), "new").unwrap();
    assert!(matches!(
        leaf.revalidate(),
        Err(InputObservationError::Changed)
    ));
    assert_eq!(leaf.file().unwrap().metadata().unwrap().len(), 8);
}

#[test]
fn input_observation_symbolic_link_target_is_a_separate_non_following_step() {
    let root = tempfile::tempdir().unwrap();
    let target = OsStr::from_bytes(b"../not-present/\xff");
    symlink(target, root.path().join("link")).unwrap();
    let link = lookup(&frontier(root.path()), "link");
    assert_eq!(link.kind(), Some(InputFileKind::SymbolicLink));
    assert!(matches!(
        link.metadata_step(),
        Err(InputObservationError::UnresolvedSymbolicLink)
    ));
    assert!(matches!(
        link.next_frontier(),
        Err(InputObservationError::UnresolvedSymbolicLink)
    ));
    let InputObservation::SymbolicLinkTarget(actual) =
        link.symbolic_link_target_step().unwrap().observe().unwrap()
    else {
        panic!("link primitive returned a different operation");
    };
    assert_eq!(actual.as_os_str(), target);
    assert!(!root.path().join("not-present").exists());
}

#[test]
fn input_observation_symbolic_link_replacement_is_recovery_before_target_read() {
    let root = tempfile::tempdir().unwrap();
    symlink("first", root.path().join("link")).unwrap();
    let link = lookup(&frontier(root.path()), "link");
    let original_step = link.symbolic_link_target_step().unwrap();
    fs::rename(root.path().join("link"), root.path().join("old-link")).unwrap();
    symlink("replacement", root.path().join("link")).unwrap();
    assert!(matches!(
        original_step.observe(),
        Err(InputObservationError::Changed)
    ));
}

#[test]
fn input_observation_terminal_metadata_reports_real_posix_facts_without_a_holder_codec() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("program"), b"original").unwrap();
    fs::set_permissions(
        root.path().join("program"),
        fs::Permissions::from_mode(0o640),
    )
    .unwrap();
    let program = lookup(&frontier(root.path()), "program");
    let InputObservation::Metadata(actual) = program.metadata_step().unwrap().observe().unwrap()
    else {
        panic!("metadata primitive returned a different operation");
    };
    let expected = fs::metadata(root.path().join("program")).unwrap();
    assert_eq!(actual.kind, InputFileKind::File);
    assert_eq!(actual.length, expected.len());
    assert_eq!(actual.mode, expected.mode());
    assert_eq!(actual.uid, expected.uid());
    assert_eq!(actual.gid, expected.gid());
    let decoded: InputMetadata =
        serde_json::from_slice(&serde_json::to_vec(&actual).unwrap()).unwrap();
    assert_eq!(decoded, actual);
}

#[test]
fn input_observation_effective_execute_preserves_os_eacces_and_success() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("program");
    fs::write(&path, "program").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let program = lookup(&frontier(root.path()), "program");
    let InputObservation::EffectiveExecute(denied) =
        program.effective_execute_step().unwrap().observe().unwrap()
    else {
        panic!("access primitive returned a different operation");
    };
    // Even root needs an execute bit on a regular file; use the real OS check.
    assert_eq!(denied, Err(libc::EACCES));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let InputObservation::EffectiveExecute(allowed) =
        program.effective_execute_step().unwrap().observe().unwrap()
    else {
        panic!("access primitive returned a different operation");
    };
    assert_eq!(allowed, Ok(()));
}

#[test]
fn input_observation_permission_lookup_keeps_the_actual_credentials_errno() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("leaf"), "original").unwrap();
    let parent = frontier(root.path());
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o000)).unwrap();
    let expected = fs::symlink_metadata(root.path().join("leaf"));
    let actual = lookup(&parent, "leaf");
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    match expected {
        Err(error) => assert_eq!(actual.errno(), error.raw_os_error()),
        // A privileged test process may legitimately bypass directory search.
        Ok(_) => assert_eq!(actual.kind(), Some(InputFileKind::File)),
    }
}

#[test]
fn input_observation_owned_step_keeps_actual_ancestor_fds_after_frontier_drop() {
    fn owned_work<T: Send + 'static>(_: &T) {}
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("leaf"), "original").unwrap();
    let parent = frontier(root.path());
    let step = parent.lookup_step(OsStr::new("leaf")).unwrap();
    owned_work(&step);
    drop(parent);
    let InputObservation::Lookup(actual) = step.observe().unwrap() else {
        panic!("lookup primitive returned a different operation");
    };
    actual.revalidate().unwrap();
    assert_eq!(actual.kind(), Some(InputFileKind::File));
}

#[test]
fn input_observation_deleted_held_object_never_turns_into_a_new_negative() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("leaf"), "original").unwrap();
    let leaf = lookup(&frontier(root.path()), "leaf");
    let step = leaf.metadata_step().unwrap();
    fs::remove_file(root.path().join("leaf")).unwrap();
    assert!(matches!(
        step.observe(),
        Err(InputObservationError::Changed)
    ));
    assert_eq!(leaf.errno(), None);
}

#[test]
fn input_observation_root_fd_does_not_claim_outer_name_stability() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("anchor")).unwrap();
    fs::write(root.path().join("anchor/original"), "retained").unwrap();
    let parent = frontier(&root.path().join("anchor"));
    fs::rename(root.path().join("anchor"), root.path().join("moved")).unwrap();
    fs::create_dir(root.path().join("anchor")).unwrap();
    parent.revalidate().unwrap();
    assert_eq!(
        lookup(&parent, "original").kind(),
        Some(InputFileKind::File)
    );
    assert!(!root.path().join("anchor/original").exists());
}

#[test]
fn input_observation_accepts_single_dot_components_and_rejects_embedded_paths() {
    let root = tempfile::tempdir().unwrap();
    let parent = frontier(root.path());
    assert_eq!(lookup(&parent, ".").kind(), Some(InputFileKind::Directory));
    assert_eq!(lookup(&parent, "..").kind(), Some(InputFileKind::Directory));
    for invalid in [b"".as_slice(), b"one/two", b"../other", b"has\0nul"] {
        assert!(matches!(
            parent.lookup_step(OsStr::from_bytes(invalid)),
            Err(InputObservationError::InvalidStep(_))
        ));
    }
}

#[test]
fn input_observation_new_mount_is_only_a_reported_frontier_fact() {
    let parent = frontier(std::path::Path::new("/"));
    let proc = lookup(&parent, "proc");
    assert_eq!(proc.kind(), Some(InputFileKind::Directory));
    assert_ne!(proc.fact().unwrap().mount, parent.fact().mount);
    // No automatic traversal, same-mount/qualification claim, or program search.
    // The future common owner must admit/refuse this next frontier separately.
    let next = proc.next_frontier().unwrap();
    assert_eq!(next.fact(), proc.fact().unwrap());
    assert_eq!(next.mount_namespace_inode(), parent.mount_namespace_inode());
    next.revalidate().unwrap();
}

#[test]
fn input_observation_retained_descriptors_are_close_on_exec() {
    use std::os::fd::AsRawFd;
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("leaf"), "original").unwrap();
    let parent = frontier(root.path());
    let leaf = lookup(&parent, "leaf");
    for file in [parent.file(), leaf.file().unwrap()] {
        // SAFETY: these borrowed descriptors stay held throughout fcntl.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0);
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }
}
