use calm_session::{
    control::{ControlMsg, ControlReply, IoMode, execution::*},
    read_frame, write_frame,
};
use std::{os::fd::AsFd, path::Path};
use workspace::execution::objects::{HeldSourceObjects, ObjectRole};
#[path = "execution_contract/support.rs"]
mod support;
use support::*;

fn transfer(
    client: &RegisteredExecutorConnection,
    registration: ExecutionRegistration,
    held: &HeldSourceObjects,
) -> AttachRegistration {
    let incarnation = match registration {
        ExecutionRegistration::Worktree { .. } => {
            SourceObjectIncarnation::Worktree(SourceIncarnationId::fresh().unwrap())
        }
        ExecutionRegistration::ExternalDir { .. } => {
            SourceObjectIncarnation::ExternalDir(ExternalDirectoryIncarnationId::fresh().unwrap())
        }
    };
    AttachRegistration {
        executor_boot: client.executor_boot().clone(),
        nonce: TransferNonce::fresh().unwrap(),
        incarnation,
        registration,
        shape: held.shape().clone(),
    }
}
async fn raw(harness: &Harness) -> tokio::net::UnixStream {
    let mut socket = tokio::net::UnixStream::connect(&harness.socket)
        .await
        .unwrap();
    write_frame(
        &mut socket,
        &ControlMsg::Execution(ExecutionRequest::Handshake(ExecutionHandshake {
            required_contract: EXECUTION_ENVELOPE_VERSION,
            client_secret: harness.secret.clone(),
            mode: ExecutionProtocolMode::RegisteredV2,
        })),
    )
    .await
    .unwrap();
    assert!(matches!(
        read_frame::<ControlReply, _>(&mut socket).await.unwrap(),
        ControlReply::Execution(ExecutionReply::Handshake {
            envelope_version: EXECUTION_ENVELOPE_VERSION,
            ..
        })
    ));
    socket
}
fn git(root: &Path, args: &[&str]) {
    let result = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "Git fixture failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[tokio::test]
async fn execution_attachment_export_retains_live_objects_after_kernel_connection_loss() {
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let root = harness.root("worktree", true);
    let grant = authority();
    let (scope, registration) = worktree(root, &grant);
    register(&mut client, registration).await;
    let original = client.registrations().last().unwrap().clone();
    drop(client); // No sender lease remains. The receiver's original open descriptions keep continuity.
    let mut recovered = harness.connect().await;
    let (receipt, held) = recovered
        .export(ExportRegistration {
            executor_boot: recovered.executor_boot().clone(),
            registration: original.id.clone(),
            nonce: TransferNonce::fresh().unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(receipt, original);
    assert_eq!(held.shape(), &original.transfer.shape);
    let request = prepare(
        &recovered,
        scope,
        grant,
        "after-kernel-restart",
        IoMode::Pipe,
        "pwd",
    );
    let native = launch(&mut recovered, &request).await;
    wait_for(&mut recovered, &request, &native, |state, output| {
        matches!(state, ExecutionState::Drained { .. }) && output.contains("worktree")
    })
    .await;
}

#[tokio::test]
async fn execution_attachment_executor_restart_is_unwitnessed_until_actual_fd_reattach() {
    let mut harness = Harness::start().await;
    let mut client = harness.connect().await;
    let root = harness.root("retained", false);
    let held = HeldSourceObjects::open(&root.path, false).unwrap();
    let grant = authority();
    let (scope, registration) = external(root, &grant);
    let original_transfer = transfer(&client, registration, &held);
    let original = client
        .attach(original_transfer.clone(), &held)
        .await
        .unwrap();
    let pending = prepare(
        &client,
        scope.clone(),
        grant.clone(),
        "pending",
        IoMode::Pipe,
        "printf wrong > forbidden",
    );
    assert!(matches!(
        client
            .request(ExecutionRequest::Prepare(pending.clone()))
            .await
            .unwrap(),
        ExecutionReply::Prepared
    ));
    drop(client);
    harness.child.kill().unwrap();
    harness.child.wait().unwrap();
    harness.child = Harness::spawn(harness.temp.path(), &harness.socket);
    let mut recovered = harness.connect().await;
    assert_ne!(recovered.executor_boot(), &original.transfer.executor_boot);
    let mut attempted = pending.clone();
    attempted.identity.execution_id = ExecutionId::fresh().unwrap();
    attempted.identity.ticket_id = LaunchTicketId::fresh().unwrap();
    attempted.identity.executor_boot = recovered.executor_boot().clone();
    assert!(matches!(
        recovered
            .request(ExecutionRequest::Prepare(attempted))
            .await
            .unwrap(),
        ExecutionReply::Refused { .. }
    ));
    assert!(matches!(
        recovered
            .request(ExecutionRequest::Launch(consume(&pending)))
            .await
            .unwrap(),
        ExecutionReply::Receipt {
            state: ExecutionState::Revoked,
            ..
        }
    ));
    let mut exporter = harness.connect().await;
    assert!(
        exporter
            .export(ExportRegistration {
                executor_boot: exporter.executor_boot().clone(),
                registration: original.id,
                nonce: TransferNonce::fresh().unwrap()
            })
            .await
            .is_err()
    );
    let mut next = original_transfer;
    next.executor_boot = recovered.executor_boot().clone();
    next.nonce = TransferNonce::fresh().unwrap();
    let current = recovered.attach(next, &held).await.unwrap();
    assert_eq!(current.transfer.incarnation, original.transfer.incarnation);
    let request = prepare(
        &recovered,
        scope,
        grant,
        "reattached",
        IoMode::Pipe,
        "printf retained",
    );
    let native = launch(&mut recovered, &request).await;
    wait_for(&mut recovered, &request, &native, |state, output| {
        matches!(state, ExecutionState::Drained { .. }) && output.contains("retained")
    })
    .await;
    assert!(!held.shape().root.join("forbidden").exists());
}

#[tokio::test]
async fn execution_attachment_rejects_missing_extra_wrong_kind_role_boot_and_nonce() {
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let root = harness.root("external", false);
    let held = HeldSourceObjects::open(&root.path, false).unwrap();
    let grant = authority();
    let (_, registration) = external(root, &grant);
    let good = transfer(&client, registration, &held);
    let ordinary = std::fs::File::create(harness.temp.path().join("file")).unwrap();
    for case in 0..5 {
        let socket = raw(&harness).await;
        let mut value = good.clone();
        value.nonce = TransferNonce::fresh().unwrap();
        let descriptor = held.descriptors()[0];
        let files = match case {
            0 => vec![],
            1 => vec![descriptor, descriptor],
            2 => vec![ordinary.as_fd()],
            _ => vec![descriptor],
        };
        if case == 3 {
            value.shape.objects[0].role = ObjectRole::GitDirectory;
        }
        if case == 4 {
            value.executor_boot = ExecutorBootId::fresh().unwrap();
        }
        rights::send(
            &socket,
            &ControlMsg::Execution(ExecutionRequest::Attach(value)),
            &files,
        )
        .await
        .unwrap();
        let (reply, files) = rights::receive::<ControlReply>(&socket).await.unwrap();
        assert!(files.is_empty());
        assert!(
            matches!(
                reply,
                ControlReply::Execution(ExecutionReply::Refused { .. })
            ),
            "accepted invalid attachment {case}: {reply:?}"
        );
    }
    let original = client.attach(good.clone(), &held).await.unwrap();
    let duplicate = client.attach(good.clone(), &held).await.unwrap();
    assert_eq!(duplicate, original);
    let socket = raw(&harness).await;
    let mut conflicting = good;
    conflicting.incarnation =
        SourceObjectIncarnation::ExternalDir(ExternalDirectoryIncarnationId::fresh().unwrap());
    rights::send(
        &socket,
        &ControlMsg::Execution(ExecutionRequest::Attach(conflicting)),
        &held.descriptors(),
    )
    .await
    .unwrap();
    assert!(matches!(
        rights::receive::<ControlReply>(&socket).await.unwrap().0,
        ControlReply::Execution(ExecutionReply::Refused { .. })
    ));
}

#[tokio::test]
async fn execution_attachment_linked_git_layout_is_transferred_and_replacement_blocks_launch() {
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let main = harness.root("main", true);
    git(
        &main.path,
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "fixture",
        ],
    );
    let linked = harness.temp.path().join("linked");
    git(
        &main.path,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ],
    );
    use std::os::unix::fs::MetadataExt;
    let stat = std::fs::metadata(&linked).unwrap();
    let directory = DirectoryRegistration {
        path: linked.clone(),
        device: stat.dev(),
        inode: stat.ino(),
    };
    let grant = authority();
    let (scope, registration) = worktree(directory, &grant);
    register(&mut client, registration).await;
    assert_eq!(
        client
            .registrations()
            .last()
            .unwrap()
            .transfer
            .shape
            .objects
            .iter()
            .map(|fact| fact.role.clone())
            .collect::<Vec<_>>(),
        vec![
            ObjectRole::Root,
            ObjectRole::GitDirectory,
            ObjectRole::GitEntry,
            ObjectRole::CommonDirectory,
            ObjectRole::CommonLink
        ]
    );
    let request = prepare(
        &client,
        scope,
        grant,
        "linked",
        IoMode::Pipe,
        "printf must-not-start",
    );
    let entry = linked.join(".git");
    let bytes = std::fs::read(&entry).unwrap();
    std::fs::rename(&entry, linked.join("old-git-entry")).unwrap();
    std::fs::write(entry, bytes).unwrap();
    assert!(matches!(
        client
            .request(ExecutionRequest::Prepare(request))
            .await
            .unwrap(),
        ExecutionReply::Refused { .. }
    ));
}
