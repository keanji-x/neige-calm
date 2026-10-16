use super::support::*;
use calm_session::control::{IoMode, execution::*};
use std::{os::unix::fs::MetadataExt, path::Path, process::Command};
use workspace::execution::objects::{
    HeldObjectComparison, HeldSourceObjects, ObjectQualificationFailure,
    ObjectQualificationUnsupported, ObjectRole,
};

fn directory(path: &Path) -> DirectoryRegistration {
    let metadata = std::fs::metadata(path).unwrap();
    DirectoryRegistration {
        path: path.into(),
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}
fn git(root: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .env_clear()
        .envs([
            ("PATH", "/usr/bin:/bin"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
        ])
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn transfer(
    client: &RegisteredExecutorConnection,
    registration: ExecutionRegistration,
    held: &HeldSourceObjects,
) -> AttachRegistration {
    AttachRegistration {
        registration_id: ObjectRegistrationId::fresh().unwrap(),
        executor_boot: client.executor_boot().clone(),
        nonce: TransferNonce::fresh().unwrap(),
        incarnation: SourceObjectIncarnation::Worktree(SourceIncarnationId::fresh().unwrap()),
        registration,
        shape: held.shape().clone(),
    }
}

#[tokio::test]
async fn execution_qualification_received_ext4_objects_reach_prepare_and_real_cwd() {
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let root = harness.root("qualified", true);
    std::fs::create_dir(root.path.join("nested")).unwrap();
    let held = HeldSourceObjects::open(&root.path, true).unwrap();
    let grant = authority();
    let (mut scope, registration) = worktree(root.clone(), &grant);
    let transfer = transfer(&client, registration, &held);
    let original = client.attach(transfer.clone(), &held).await.unwrap();
    assert_eq!(
        client.attach(transfer, &held).await.unwrap(),
        original,
        "exact Attach replay retains the immutable receipt"
    );
    let (_, received) = client
        .export(ExportRegistration {
            executor_boot: client.executor_boot().clone(),
            registration: original.id,
            nonce: TransferNonce::fresh().unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(
        held.live_identity()
            .unwrap()
            .compare(&received.live_identity().unwrap()),
        HeldObjectComparison::SameObject
    );
    if let ExecutionScope::Worktree { cwd, .. } = &mut scope {
        *cwd = workspace::identity::WorkspaceCwd::try_from("nested".to_owned()).unwrap();
    }
    let request = prepare(&client, scope, grant, "qualified-cwd", IoMode::Pipe, "pwd");
    let native = launch(&mut client, &request).await;
    wait_for(&mut client, &request, &native, |state, output| {
        matches!(state, ExecutionState::Drained { .. }) && output.contains("qualified/nested")
    })
    .await;
}

#[tokio::test]
async fn execution_qualification_attach_replay_retains_original_open_description() {
    use std::os::fd::AsRawFd;
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let root = harness.root("retained-description", true);
    let original = HeldSourceObjects::open(&root.path, true).unwrap();
    let grant = authority();
    let (_, registration) = worktree(root.clone(), &grant);
    let attachment = transfer(&client, registration, &original);
    // Linux flock belongs to an open file description. SCM_RIGHTS preserves that reference;
    // a fresh open of the same inode cannot acquire this lock while the original remains held.
    assert_eq!(
        unsafe { libc::flock(original.root().as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let receipt = client.attach(attachment.clone(), &original).await.unwrap();
    let fresh = HeldSourceObjects::open(&root.path, true).unwrap();
    assert_eq!(client.attach(attachment, &fresh).await.unwrap(), receipt);
    let (_, exported) = client
        .export(ExportRegistration {
            executor_boot: client.executor_boot().clone(),
            registration: receipt.id,
            nonce: TransferNonce::fresh().unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(exported.root().as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0,
        "exact Attach replay replaced the original held open description"
    );
    assert_eq!(
        unsafe { libc::flock(original.root().as_raw_fd(), libc::LOCK_UN) },
        0
    );
}

#[tokio::test]
async fn execution_qualification_unsupported_worktree_does_not_disable_external_directory() {
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let unsupported = tempfile::tempdir_in("/dev/shm").unwrap();
    git(unsupported.path(), &["init", "-q"]);
    let held = HeldSourceObjects::open(unsupported.path(), true).unwrap();
    let grant = authority();
    let (scope, registration) = worktree(directory(unsupported.path()), &grant);
    let attachment = transfer(&client, registration, &held);
    let receipt = client.attach(attachment.clone(), &held).await.unwrap();
    assert_eq!(
        client.attach(attachment, &held).await.unwrap(),
        receipt,
        "Attach is retained facts, not a qualification fallback"
    );
    let request = prepare(
        &client,
        scope,
        grant.clone(),
        "unsupported-worktree",
        IoMode::Pipe,
        "touch forbidden",
    );
    for _ in 0..2 {
        assert!(matches!(
            client
                .request(ExecutionRequest::Prepare(request.clone()))
                .await
                .unwrap(),
            ExecutionReply::ObjectQualificationFailed {
                failure: ObjectQualificationFailure::Unsupported(
                    ObjectQualificationUnsupported::Filesystem {
                        role: ObjectRole::Root,
                        magic: 0x01021994,
                        ..
                    }
                )
            }
        ));
    }
    assert!(!unsupported.path().join("forbidden").exists());
    let (scope, registration) = external(directory(unsupported.path()), &grant);
    register(&mut client, registration).await;
    let external = prepare(
        &client,
        scope,
        grant,
        "explicit-external",
        IoMode::Pipe,
        "printf external-ok",
    );
    let native = launch(&mut client, &external).await;
    wait_for(&mut client, &external, &native, |state, output| {
        matches!(state, ExecutionState::Drained { .. }) && output.contains("external-ok")
    })
    .await;
}

#[tokio::test]
async fn execution_qualification_each_git_role_requires_its_own_filesystem() {
    let harness = Harness::start().await;
    let root = harness.temp.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let unsupported = tempfile::tempdir_in("/dev/shm").unwrap();
    let gitdir = unsupported.path().join("git");
    git(
        &root,
        &["init", "-q", "--separate-git-dir", gitdir.to_str().unwrap()],
    );
    let held = HeldSourceObjects::open(&root, true).unwrap();
    assert!(matches!(
        held.qualify_local(),
        Err(ObjectQualificationFailure::Unsupported(
            ObjectQualificationUnsupported::Filesystem {
                role: ObjectRole::GitDirectory,
                ..
            }
        ))
    ));
    let mut client = harness.connect().await;
    let grant = authority();
    let (scope, registration) = worktree(directory(&root), &grant);
    client
        .attach(transfer(&client, registration, &held), &held)
        .await
        .unwrap();
    let request = prepare(
        &client,
        scope,
        grant,
        "git-role",
        IoMode::Pipe,
        "touch forbidden",
    );
    assert!(matches!(
        client
            .request(ExecutionRequest::Prepare(request))
            .await
            .unwrap(),
        ExecutionReply::ObjectQualificationFailed {
            failure: ObjectQualificationFailure::Unsupported(
                ObjectQualificationUnsupported::Filesystem {
                    role: ObjectRole::GitDirectory,
                    ..
                }
            )
        }
    ));
    assert!(!root.join("forbidden").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn execution_qualification_real_namespace_mount_changes_and_fork_boundary() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    let source = temporary.path().join("source");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(root.join("mounted")).unwrap();
    let original_mountpoint = std::fs::metadata(root.join("mounted")).unwrap().ino();
    let source_inode = std::fs::metadata(&source).unwrap().ino();
    assert_ne!(original_mountpoint, source_inode);
    git(&root, &["init", "-q"]);
    let output = Command::new(env!("CARGO_BIN_EXE_proc-supervisor-foreground-job"))
        .arg("--probe-object-qualification")
        .args([
            &root,
            &source,
            Path::new(env!("CARGO_BIN_EXE_calm-proc-supervisor")),
        ])
        .env_clear()
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "required private namespace qualification failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    for field in [
        "namespace_changed_unknown",
        "namespace_comparison_unsupported",
        "prepare_before_bind",
        "launch_cross_mount_unsupported",
        "cwd_cross_mount_unsupported",
        "changed_mount_unknown",
        "lost_mount_unknown",
        "no_payload",
    ] {
        assert_eq!(report[field], true, "{field}: {report}");
    }
    assert_eq!(
        std::fs::metadata(root.join("mounted")).unwrap().ino(),
        original_mountpoint,
        "bind escaped the disposable child's namespace"
    );
    assert!(!root.join("mounted/forbidden").exists());
    assert!(
        !source.join("forbidden").exists(),
        "scratch bind must not escape its child mount namespace"
    );
}
