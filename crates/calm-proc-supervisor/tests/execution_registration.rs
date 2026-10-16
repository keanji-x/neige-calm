use calm_session::control::{IoMode, ProcSignal, execution::*};
use workspace::execution::objects::{ContainmentKind, DirectoryContainment, HeldSourceObjects};
#[path = "execution_contract/support.rs"]
mod support;
use support::*;

struct Pair {
    harness: Harness,
    left: RegisteredExecutorConnection,
    right: RegisteredExecutorConnection,
    held: HeldSourceObjects,
    first: PrepareExecution,
    second: PrepareExecution,
    first_receipt: ExecutionReceipt,
    second_receipt: ExecutionReceipt,
    second_transfer: AttachRegistration,
}
async fn launch_prepared(
    client: &mut RegisteredExecutorConnection,
    request: &PrepareExecution,
) -> ExecutionReceipt {
    match client
        .request(ExecutionRequest::Launch(consume(request)))
        .await
        .unwrap()
    {
        ExecutionReply::Receipt {
            receipt: Some(receipt),
            state: ExecutionState::Spawned,
        } => receipt,
        other => panic!("prepared operation did not fork: {other:?}"),
    }
}
async fn pair() -> Pair {
    let harness = Harness::start().await;
    let mut left = harness.connect().await;
    let mut right = harness.connect().await;
    let root = harness.root("shared", true);
    let held = HeldSourceObjects::open(&root.path, true).unwrap();
    let first_grant = authority();
    let (scope, registration) = worktree(root, &first_grant);
    let first_transfer = AttachRegistration {
        registration_id: ObjectRegistrationId::fresh().unwrap(),
        executor_boot: left.executor_boot().clone(),
        nonce: TransferNonce::fresh().unwrap(),
        incarnation: SourceObjectIncarnation::Worktree(SourceIncarnationId::fresh().unwrap()),
        registration,
        shape: held.shape().clone(),
    };
    left.attach(first_transfer.clone(), &held).await.unwrap();
    let command = "printf ready; read first; printf 'accepted:%s\\n' \"$first\"; read last";
    let first = prepare(
        &left,
        scope.clone(),
        first_grant,
        "first",
        IoMode::Pipe,
        command,
    );
    assert!(matches!(
        left.request(ExecutionRequest::Prepare(first.clone()))
            .await
            .unwrap(),
        ExecutionReply::Prepared
    ));
    let second_grant = authority();
    let mut second_transfer = first_transfer;
    second_transfer.registration_id = ObjectRegistrationId::fresh().unwrap();
    second_transfer.nonce = TransferNonce::fresh().unwrap();
    match &mut second_transfer.registration {
        ExecutionRegistration::Worktree { authority, .. } => *authority = second_grant.clone(),
        _ => unreachable!(),
    }
    // Same source, binding/version, generation and epoch; only this operation's accepted grant differs.
    right.attach(second_transfer.clone(), &held).await.unwrap();
    let second = prepare(&right, scope, second_grant, "second", IoMode::Pipe, command);
    assert!(matches!(
        right
            .request(ExecutionRequest::Prepare(second.clone()))
            .await
            .unwrap(),
        ExecutionReply::Prepared
    ));
    let first_receipt = launch_prepared(&mut left, &first).await;
    let second_receipt = launch_prepared(&mut right, &second).await;
    wait_for(&mut left, &first, &first_receipt, |_, output| {
        output.contains("ready")
    })
    .await;
    wait_for(&mut right, &second, &second_receipt, |_, output| {
        output.contains("ready")
    })
    .await;
    Pair {
        harness,
        left,
        right,
        held,
        first,
        second,
        first_receipt,
        second_receipt,
        second_transfer,
    }
}
async fn input(
    client: &mut RegisteredExecutorConnection,
    request: &PrepareExecution,
    receipt: &ExecutionReceipt,
    bytes: &[u8],
) -> ExecutionReply {
    client
        .request(ExecutionRequest::Control(ExecutionControl {
            capability: capability(request, receipt, ExecutionControlPurpose::Interactive),
            action: ProcessAction::WriteStdin {
                bytes: bytes.into(),
            },
        }))
        .await
        .unwrap()
}
async fn stop(
    client: &mut RegisteredExecutorConnection,
    request: &PrepareExecution,
    receipt: &ExecutionReceipt,
) {
    assert!(matches!(
        client
            .request(ExecutionRequest::Control(ExecutionControl {
                capability: capability(request, receipt, ExecutionControlPurpose::Lifecycle),
                action: ProcessAction::Signal(ProcSignal::Kill),
            }))
            .await
            .unwrap(),
        ExecutionReply::ActionComplete
    ));
    wait_for(client, request, receipt, |state, _| {
        matches!(state, ExecutionState::Drained { .. })
    })
    .await;
}

#[tokio::test]
async fn execution_same_binding_operation_grants_preserve_both_pending_and_live_receipts() {
    let mut pair = pair().await;
    let mut forged = pair.second.clone();
    forged.identity.execution_id = ExecutionId::fresh().unwrap();
    forged.identity.ticket_id = LaunchTicketId::fresh().unwrap();
    forged.identity.source_registration = pair.first.identity.source_registration.clone();
    assert!(matches!(
        pair.right
            .request(ExecutionRequest::Prepare(forged))
            .await
            .unwrap(),
        ExecutionReply::Refused { .. }
    ));
    let other = pair.harness.root("other", false);
    let other_held = HeldSourceObjects::open(&other.path, false).unwrap();
    let (_, other_registration) = external(other, &authority());
    let collision = AttachRegistration {
        registration_id: pair.first.identity.source_registration.clone(),
        executor_boot: pair.right.executor_boot().clone(),
        nonce: TransferNonce::fresh().unwrap(),
        incarnation: SourceObjectIncarnation::ExternalDir(
            ExternalDirectoryIncarnationId::fresh().unwrap(),
        ),
        registration: other_registration,
        shape: other_held.shape().clone(),
    };
    assert!(pair.right.attach(collision, &other_held).await.is_err());
    assert!(matches!(
        input(&mut pair.left, &pair.first, &pair.first_receipt, b"left\n").await,
        ExecutionReply::ActionComplete
    ));
    assert!(matches!(
        input(
            &mut pair.right,
            &pair.second,
            &pair.second_receipt,
            b"right\n"
        )
        .await,
        ExecutionReply::ActionComplete
    ));
    wait_for(
        &mut pair.left,
        &pair.first,
        &pair.first_receipt,
        |_, output| output.contains("accepted:left"),
    )
    .await;
    wait_for(
        &mut pair.right,
        &pair.second,
        &pair.second_receipt,
        |_, output| output.contains("accepted:right"),
    )
    .await;
    let mut close = pair.second_transfer.clone();
    close.registration_id = ObjectRegistrationId::fresh().unwrap();
    close.nonce = TransferNonce::fresh().unwrap();
    match &mut close.registration {
        ExecutionRegistration::Worktree {
            binding_version,
            admission_open,
            ..
        } => {
            *binding_version += 1;
            *admission_open = false;
        }
        _ => unreachable!(),
    }
    pair.right.attach(close, &pair.held).await.unwrap();
    assert!(matches!(
        input(&mut pair.left, &pair.first, &pair.first_receipt, b"late\n").await,
        ExecutionReply::Refused { .. }
    ));
    assert!(matches!(
        input(
            &mut pair.right,
            &pair.second,
            &pair.second_receipt,
            b"late\n"
        )
        .await,
        ExecutionReply::Refused { .. }
    ));
    stop(&mut pair.left, &pair.first, &pair.first_receipt).await;
    stop(&mut pair.right, &pair.second, &pair.second_receipt).await;
}

#[tokio::test]
async fn execution_exact_revocation_is_durable_without_revoking_another_operation() {
    let mut pair = pair().await;
    let journal = pair.harness.journal();
    let moved = pair.harness.temp.path().join("moved-journal");
    struct Restore(std::path::PathBuf, std::path::PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            if self.0.exists() {
                std::fs::rename(&self.0, &self.1).unwrap();
            }
        }
    }
    std::fs::rename(&journal, &moved).unwrap();
    let restore = Restore(moved, journal.clone());
    assert!(matches!(
        pair.left
            .request(ExecutionRequest::Revoke(consume(&pair.first)))
            .await
            .unwrap(),
        ExecutionReply::Refused { .. }
    ));
    assert!(matches!(
        input(
            &mut pair.left,
            &pair.first,
            &pair.first_receipt,
            b"forbidden\n"
        )
        .await,
        ExecutionReply::Refused { .. }
    ));
    assert!(matches!(
        input(
            &mut pair.right,
            &pair.second,
            &pair.second_receipt,
            b"peer\n"
        )
        .await,
        ExecutionReply::ActionComplete
    ));
    wait_for(
        &mut pair.right,
        &pair.second,
        &pair.second_receipt,
        |_, output| output.contains("accepted:peer"),
    )
    .await;
    drop(restore);
    for _ in 0..2 {
        match pair
            .left
            .request(ExecutionRequest::Revoke(consume(&pair.first)))
            .await
            .unwrap()
        {
            ExecutionReply::RevocationAcknowledged {
                execution_id,
                ticket_id,
                receipt: Some(receipt),
                state: ExecutionState::Spawned,
            } => {
                assert_eq!(execution_id, pair.first.identity.execution_id);
                assert_eq!(ticket_id, pair.first.identity.ticket_id);
                assert_eq!(receipt, pair.first_receipt);
            }
            other => panic!("revocation was not durably acknowledged: {other:?}"),
        }
    }
    let saved: serde_json::Value = serde_json::from_slice(
        &std::fs::read(journal.join(format!(
            "{}.execution",
            pair.first.identity.execution_id.as_str()
        )))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(saved["interactive_revoked"], true);
    assert!(matches!(
        input(
            &mut pair.left,
            &pair.first,
            &pair.first_receipt,
            b"still-forbidden\n"
        )
        .await,
        ExecutionReply::Refused { .. }
    ));
    match pair
        .left
        .request(ExecutionRequest::Launch(consume(&pair.first)))
        .await
        .unwrap()
    {
        ExecutionReply::Receipt {
            receipt: Some(receipt),
            state: ExecutionState::Spawned,
        } => assert_eq!(receipt, pair.first_receipt),
        other => panic!("original receipt replay changed after revocation: {other:?}"),
    }
    stop(&mut pair.left, &pair.first, &pair.first_receipt).await;
    stop(&mut pair.right, &pair.second, &pair.second_receipt).await;
}

#[tokio::test]
async fn execution_directory_containment_uses_actual_target_and_never_infers_disjoint() {
    let harness = Harness::start().await;
    let root = harness.root("parent", true);
    let parent = HeldSourceObjects::open(&root.path, true).unwrap();
    let child_path = root.path.join("child");
    std::fs::create_dir(&child_path).unwrap();
    let child = HeldSourceObjects::open(&child_path, false).unwrap();
    let mut client = harness.connect().await;
    let grant = authority();
    let (mut scope, registration) = worktree(root.clone(), &grant);
    register(&mut client, registration).await;
    if let ExecutionScope::Worktree { cwd, .. } = &mut scope {
        *cwd = workspace::identity::WorkspaceCwd::try_from("child".to_owned()).unwrap();
    }
    let request = prepare(&client, scope, grant, "relative-cwd", IoMode::Pipe, "pwd");
    let receipt = launch(&mut client, &request).await;
    let (_, output) = wait_for(&mut client, &request, &receipt, |state, output| {
        matches!(state, ExecutionState::Drained { .. }) && output.contains("child")
    })
    .await;
    assert!(
        output.contains(child_path.to_str().unwrap()),
        "actual FD cwd resolver chose another directory: {output}"
    );
    match parent.contains_directory(&child) {
        DirectoryContainment::Verified(proof) => {
            assert_eq!(proof.kind(), ContainmentKind::StrictDescendant);
            assert_eq!(
                proof.ancestor_key(),
                parent.live_identity().unwrap().root_key()
            );
            assert_eq!(
                proof.subject_key(),
                child.live_identity().unwrap().root_key()
            );
        }
        _ => panic!("same-mount Linux openat2 did not verify held descendant"),
    }
    assert!(
        matches!(parent.contains_directory(&parent), DirectoryContainment::Verified(proof) if proof.kind() == ContainmentKind::SameDirectory)
    );
    let sibling_root = harness.root("sibling", false);
    let sibling = HeldSourceObjects::open(&sibling_root.path, false).unwrap();
    assert!(matches!(
        parent.contains_directory(&sibling),
        DirectoryContainment::Unknown(_)
    ));
    // A still-matching path prefix is no authority when it now resolves to another object.
    std::fs::rename(&child_path, harness.temp.path().join("moved-child")).unwrap();
    std::fs::create_dir(&child_path).unwrap();
    assert!(matches!(
        parent.contains_directory(&child),
        DirectoryContainment::Unknown(_)
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn execution_bind_mount_alias_cannot_allocate_another_physical_root_bucket() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    let alias = temporary.path().join("alias");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&alias).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_proc-supervisor-foreground-job"))
        .arg("--probe-bind-alias")
        .args([&root, &alias])
        .env_clear()
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "real bind capability probe failed; cannot qualify this host: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let observed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(observed["same_physical_root"], true, "{observed}");
    assert_eq!(observed["different_mount"], true, "{observed}");
    assert_eq!(
        observed["same_bucket"], true,
        "bind alias split the physical root index: {observed}"
    );
    assert_eq!(
        observed["same_bucket_hash"], true,
        "equal physical roots hashed into different index buckets: {observed}"
    );
    assert_eq!(
        observed["declared_different_root"], false,
        "bind alias was guessed disjoint: {observed}"
    );
    assert_eq!(
        observed["comparison"], "MountAliasUnsupported",
        "different mount views were silently joined: {observed}"
    );
    use std::os::unix::fs::MetadataExt;
    let original = std::fs::metadata(&root).unwrap();
    let untouched = std::fs::metadata(&alias).unwrap();
    assert_ne!(
        (original.dev(), original.ino()),
        (untouched.dev(), untouched.ino()),
        "bind escaped the disposable child mount namespace"
    );
}
