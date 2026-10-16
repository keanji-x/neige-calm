use super::support::*;
use calm_session::{
    control::{ControlMsg, ControlReply, IoMode, execution::*},
    read_frame, write_frame,
};
use std::{os::unix::fs::MetadataExt, path::Path, time::Duration};
use workspace::execution::objects::{
    ContainmentKind, DirectoryRelationOutcome, DirectoryRelationUnknown, ObjectRole,
};

fn query(
    client: &RegisteredExecutorConnection,
    ancestor: &ObjectRegistrationReceipt,
    subject: &ObjectRegistrationReceipt,
) -> DirectoryRelationRequest {
    DirectoryRelationRequest {
        executor_boot: client.executor_boot().clone(),
        nonce: TransferNonce::fresh().unwrap(),
        ancestor: RegisteredDirectoryRoot {
            receipt: ancestor.clone(),
            role: ObjectRole::Root,
        },
        subject: RegisteredDirectoryRoot {
            receipt: subject.clone(),
            role: ObjectRole::Root,
        },
    }
}

fn directory(path: &Path) -> DirectoryRegistration {
    let stat = std::fs::metadata(path).unwrap();
    DirectoryRegistration {
        path: path.into(),
        device: stat.dev(),
        inode: stat.ino(),
    }
}

async fn attach_external(
    client: &mut RegisteredExecutorConnection,
    root: DirectoryRegistration,
) -> ObjectRegistrationReceipt {
    let (_, registration) = external(root, &authority());
    register(client, registration).await;
    client.registrations().last().unwrap().clone()
}

#[tokio::test]
async fn execution_directory_relation_real_helper_qualifies_same_descendant_and_sibling() {
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let parent = harness.root("source", true);
    std::fs::create_dir(parent.path.join("nested")).unwrap();
    let (_, registration) = worktree(parent.clone(), &authority());
    register(&mut client, registration).await;
    let root = client.registrations().last().unwrap().clone();
    let nested = attach_external(&mut client, directory(&parent.path.join("nested"))).await;
    let sibling = attach_external(&mut client, harness.root("outside", false)).await;
    for (subject, expected) in [
        (
            &root,
            DirectoryRelationOutcome::Contains(ContainmentKind::SameDirectory),
        ),
        (
            &nested,
            DirectoryRelationOutcome::Contains(ContainmentKind::StrictDescendant),
        ),
        (&sibling, DirectoryRelationOutcome::Disjoint),
    ] {
        let request = query(&client, &root, subject);
        let observation = client.observe_directories(request.clone()).await.unwrap();
        assert_eq!(observation.request(), &request);
        assert_eq!(
            observation.outcome(),
            &expected,
            "required host qualification failed; missing userns/chroot capability is not a skip"
        );
    }
}

#[tokio::test]
async fn execution_directory_relation_rename_and_delete_follow_retained_objects() {
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let parent = harness.root("source", false);
    let child_path = parent.path.join("child");
    std::fs::create_dir(&child_path).unwrap();
    let root = attach_external(&mut client, parent.clone()).await;
    let child = attach_external(&mut client, directory(&child_path)).await;
    let outside = harness.temp.path().join("moved");
    std::fs::rename(&child_path, &outside).unwrap();
    let request = query(&client, &root, &child);
    assert_eq!(
        client.observe_directories(request).await.unwrap().outcome(),
        &DirectoryRelationOutcome::Disjoint
    );
    let renamed_inside = parent.path.join("another-name");
    std::fs::rename(&outside, &renamed_inside).unwrap();
    let request = query(&client, &root, &child);
    assert_eq!(
        client.observe_directories(request).await.unwrap().outcome(),
        &DirectoryRelationOutcome::Contains(ContainmentKind::StrictDescendant)
    );
    std::fs::remove_dir(&renamed_inside).unwrap();
    let request = query(&client, &root, &child);
    assert_eq!(
        client.observe_directories(request).await.unwrap().outcome(),
        &DirectoryRelationOutcome::Unknown(DirectoryRelationUnknown::DetachedDirectory)
    );
}

#[tokio::test]
async fn execution_directory_relation_rejects_unwitnessed_boot_role_and_immutable_receipt() {
    let mut harness = Harness::start().await;
    let mut client = harness.connect().await;
    let root = attach_external(&mut client, harness.root("object", false)).await;
    let valid = query(&client, &root, &root);
    let mut invalid = Vec::new();
    let mut wrong = valid.clone();
    wrong.executor_boot = ExecutorBootId::fresh().unwrap();
    invalid.push(wrong);
    let mut wrong = valid.clone();
    wrong.subject.role = ObjectRole::GitDirectory;
    invalid.push(wrong);
    let mut wrong = valid.clone();
    wrong.subject.receipt.id = ObjectRegistrationId::fresh().unwrap();
    invalid.push(wrong);
    let mut wrong = valid.clone();
    wrong.subject.receipt.transfer.nonce = TransferNonce::fresh().unwrap();
    invalid.push(wrong);
    let mut wrong = valid.clone();
    wrong.subject.receipt.transfer.incarnation =
        SourceObjectIncarnation::ExternalDir(ExternalDirectoryIncarnationId::fresh().unwrap());
    invalid.push(wrong);
    for request in invalid {
        assert!(matches!(
            client
                .request(ExecutionRequest::ObserveDirectories(request))
                .await
                .unwrap(),
            ExecutionReply::Refused { .. }
        ));
    }
    drop(client);
    harness.child.kill().unwrap();
    harness.child.wait().unwrap();
    harness.child = Harness::spawn(harness.temp.path(), &harness.socket);
    let mut restarted = harness.connect().await;
    let mut history = valid;
    history.executor_boot = restarted.executor_boot().clone();
    assert!(
        matches!(
            restarted
                .request(ExecutionRequest::ObserveDirectories(history))
                .await
                .unwrap(),
            ExecutionReply::Refused { .. }
        ),
        "a deserialized registration must not recreate a live root after executor boot loss"
    );
}

#[tokio::test]
async fn execution_directory_relation_old_held_receipt_is_observable_but_cannot_launch() {
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let grant = authority();
    let (scope, registration) = worktree(harness.root("source", true), &grant);
    register(&mut client, registration.clone()).await;
    let original = client.registrations().last().unwrap().clone();
    let mut closed = registration;
    if let ExecutionRegistration::Worktree {
        binding_version,
        admission_open,
        ..
    } = &mut closed
    {
        *binding_version += 1;
        *admission_open = false;
    }
    register(&mut client, closed).await;
    let request = query(&client, &original, &original);
    assert_eq!(
        client.observe_directories(request).await.unwrap().outcome(),
        &DirectoryRelationOutcome::Contains(ContainmentKind::SameDirectory)
    );
    let mut launch = prepare(
        &client,
        scope,
        grant,
        "stale-observer",
        IoMode::Pipe,
        "touch forbidden",
    );
    launch.identity.source_registration = original.id;
    launch.identity.source_incarnation = original.transfer.incarnation;
    assert!(matches!(
        client
            .request(ExecutionRequest::Prepare(launch))
            .await
            .unwrap(),
        ExecutionReply::Refused { .. }
    ));
    assert!(!harness.temp.path().join("source/forbidden").exists());
}

#[tokio::test]
async fn execution_directory_relation_lost_reply_does_not_create_an_execution_or_replay_a_fact() {
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let root = attach_external(&mut client, harness.root("source", false)).await;
    let original = query(&client, &root, &root);
    let mut stream = tokio::net::UnixStream::connect(&harness.socket)
        .await
        .unwrap();
    write_frame(
        &mut stream,
        &ControlMsg::Execution(ExecutionRequest::Handshake(ExecutionHandshake {
            required_contract: EXECUTION_ENVELOPE_VERSION,
            client_secret: harness.secret.clone(),
            mode: ExecutionProtocolMode::RegisteredV3,
        })),
    )
    .await
    .unwrap();
    assert!(matches!(
        read_frame::<ControlReply, _>(&mut stream).await.unwrap(),
        ControlReply::Execution(ExecutionReply::Handshake { .. })
    ));
    rights::send(
        &stream,
        &ControlMsg::Execution(ExecutionRequest::ObserveDirectories(original.clone())),
        &[],
    )
    .await
    .unwrap();
    drop(stream);
    let retried = query(&client, &root, &root);
    assert_ne!(retried.nonce, original.nonce);
    let observation = client.observe_directories(retried.clone()).await.unwrap();
    assert_eq!(observation.request(), &retried);
    assert_eq!(
        observation.outcome(),
        &DirectoryRelationOutcome::Contains(ContainmentKind::SameDirectory)
    );
    assert!(!std::fs::read_dir(harness.journal()).unwrap().any(|entry| {
        entry
            .unwrap()
            .path()
            .extension()
            .is_some_and(|extension| extension == "execution")
    }));
}

#[tokio::test]
async fn execution_directory_relation_timeout_and_child_failure_are_unknown_and_reaped() {
    for (program, expected) in [
        (
            Path::new(env!("CARGO_BIN_EXE_proc-supervisor-foreground-job")),
            DirectoryRelationUnknown::Timeout,
        ),
        (
            Path::new(env!("CARGO_BIN_EXE_proc-supervisor-ready-sleeper")),
            DirectoryRelationUnknown::HelperExited,
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("control.sock");
        let secret = CapabilityToken::fresh().unwrap();
        let registry = calm_proc_supervisor::ProcRegistry::new()
            .with_executor(calm_proc_supervisor::execution::ExecutorConfig {
                journal_directory: temp.path().join("journal"),
                helper_program: program.to_owned(),
                kernel_client_secret: secret.clone(),
            })
            .unwrap();
        let listener = calm_proc_supervisor::bind_control_listener(&socket).unwrap();
        let (shutdown, receiver) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(calm_proc_supervisor::serve_with_listener(
            listener,
            socket.clone(),
            registry,
            receiver,
        ));
        let mut client = RegisteredExecutorConnection::connect(&socket, secret)
            .await
            .unwrap();
        let path = temp.path().join("root");
        std::fs::create_dir(&path).unwrap();
        let root = attach_external(&mut client, directory(&path)).await;
        let request = query(&client, &root, &root);
        let started = tokio::time::Instant::now();
        assert_eq!(
            client.observe_directories(request).await.unwrap().outcome(),
            &DirectoryRelationOutcome::Unknown(expected)
        );
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "readonly observer did not honor its bounded RPC"
        );
        // This isolated nextest process has no other child/fork entry. Nonconsuming ECHILD is the
        // kernel fact; a /proc child snapshot would not prove the timed-out helper was collected.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe {
                libc::waitid(
                    libc::P_ALL,
                    0,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
        let _ = shutdown.send(());
        server.await.unwrap().unwrap();
    }
}
