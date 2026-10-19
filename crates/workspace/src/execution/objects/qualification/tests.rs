use super::*;
use crate::{
    execution::objects::{DirectoryContainment, HeldObjectComparison},
    identity::WorkspaceCwd,
};
use std::{
    os::fd::{AsRawFd, OwnedFd},
    process::Command,
};

#[cfg(target_os = "linux")]
#[test]
fn object_qualification_real_ext4_roles_and_received_fds_keep_live_context() {
    let temporary = tempfile::tempdir().unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(temporary.path())
            .status()
            .unwrap()
            .success()
    );
    let git = |args: &[&str]| {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(temporary.path())
                .args(args)
                .status()
                .unwrap()
                .success()
        )
    };
    git(&[
        "-c",
        "user.name=test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "--allow-empty",
        "-qm",
        "fixture",
    ]);
    let linked = temporary.path().join("linked");
    git(&["worktree", "add", "-qb", "linked", linked.to_str().unwrap()]);
    let held = HeldSourceObjects::open(&linked, true).unwrap();
    let qualified = held
        .qualify_local()
        .expect("the local qualification contract requires an actual ext4 fixture");
    assert_eq!(qualified.facts().len(), held.shape().objects.len());
    assert!(
        qualified
            .facts()
            .iter()
            .all(|fact| fact.profile == LocalObjectProfile::Ext4DriverHeldV1)
    );
    let received = HeldSourceObjects::receive(
        held.shape().clone(),
        held.files
            .iter()
            .map(|file| OwnedFd::from(file.try_clone().unwrap()))
            .collect(),
    )
    .unwrap();
    assert_eq!(
        held.live_identity()
            .unwrap()
            .compare(&received.live_identity().unwrap()),
        HeldObjectComparison::SameObject
    );
    assert_eq!(
        held.descriptors().len(),
        held.shape().objects.len(),
        "passive namespace context is not an exported object role"
    );
    let flags = unsafe { libc::fcntl(held.namespace.file.as_raw_fd(), libc::F_GETFD) };
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
    qualified
        .open_cwd(&WorkspaceCwd::root())
        .unwrap()
        .revalidate()
        .unwrap();
    let entry = linked.join(".git");
    let bytes = std::fs::read(&entry).unwrap();
    std::fs::rename(&entry, linked.join("old-entry")).unwrap();
    std::fs::write(&entry, bytes).unwrap();
    let changed = HeldSourceObjects::open(&linked, true).unwrap();
    assert_eq!(
        received
            .live_identity()
            .unwrap()
            .compare(&changed.live_identity().unwrap()),
        HeldObjectComparison::LayoutConflict
    );
    assert!(received.validate().is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn object_qualification_rename_replacement_never_reopens_an_old_identity() {
    let temporary = tempfile::tempdir().unwrap();
    let original = temporary.path().join("root");
    std::fs::create_dir(&original).unwrap();
    let held = HeldSourceObjects::open(&original, false).unwrap();
    let original_identity = held.live_identity().unwrap();
    std::fs::rename(&original, temporary.path().join("retained")).unwrap();
    std::fs::create_dir(&original).unwrap();
    let replacement = HeldSourceObjects::open(&original, false).unwrap();
    assert_eq!(
        original_identity.compare(&held.live_identity().unwrap()),
        HeldObjectComparison::SameObject
    );
    assert_eq!(
        original_identity.compare(&replacement.live_identity().unwrap()),
        HeldObjectComparison::DifferentRoot
    );
    assert!(
        held.validate().is_err(),
        "named-root validation must still refuse a replacement"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn object_qualification_tmpfs_facts_do_not_produce_strong_identity() {
    let temporary = tempfile::tempdir_in("/dev/shm").unwrap();
    let held = HeldSourceObjects::open(temporary.path(), false).unwrap();
    assert!(
        held.root().metadata().unwrap().is_dir(),
        "unqualified facts remain usable"
    );
    assert!(
        matches!(held.qualify_local(), Err(ObjectQualificationFailure::Unsupported(ObjectQualificationUnsupported::Filesystem { role: ObjectRole::Root, magic: 0x01021994, filesystem })) if filesystem == "tmpfs")
    );
    assert!(held.live_identity().is_err());
    assert!(
        matches!(
            held.contains_directory(&held),
            DirectoryContainment::Unsupported(_)
        ),
        "bare dev/ino equality cannot manufacture qualified SameDirectory"
    );
}

#[test]
fn object_qualification_missing_and_invalid_mount_records_are_unknown_only() {
    let table =
        MountTable::parse("17 1 8:1 / /scratch\\040directory rw shared:2 - ext4 /dev/test rw\n")
            .unwrap();
    assert_eq!(table.get(17).unwrap().filesystem, "ext4");
    assert!(matches!(
        table.get(18),
        Err(ObjectQualificationFailure::Unknown(
            ObjectQualificationUnknown::MountInformationMissing { mount: 18 }
        ))
    ));
    for invalid in [
        "",
        "17 malformed\n",
        "17 1 8:1 / /x rw - ext4 source rw\n17 1 8:1 / /y rw - ext4 source rw\n",
    ] {
        // Parsing is input validation only; no fake record is passed to the live qualifier.
        let parsed = MountTable::parse(invalid);
        assert!(parsed.is_err() || parsed.unwrap().get(17).is_err());
    }
}

#[test]
fn object_qualification_git_directory_requires_its_own_filesystem() {
    let root = tempfile::tempdir().unwrap();
    let unsupported = tempfile::tempdir_in("/dev/shm").unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q", "--separate-git-dir"])
            .arg(unsupported.path().join("git"))
            .arg(root.path())
            .status()
            .unwrap()
            .success()
    );
    let held = HeldSourceObjects::open(root.path(), true).unwrap();
    assert!(matches!(
        held.qualify_local(),
        Err(ObjectQualificationFailure::Unsupported(
            ObjectQualificationUnsupported::Filesystem {
                role: ObjectRole::GitDirectory,
                ..
            }
        ))
    ));
}
