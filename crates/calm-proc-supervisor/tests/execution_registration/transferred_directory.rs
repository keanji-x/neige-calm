use super::support::*;
use calm_session::control::execution::*;
use std::fs::File;
use workspace::execution::objects::{
    ContainmentKind, DirectoryRelationOutcome, qualify_directory_pair,
};

#[tokio::test]
async fn execution_transferred_directories_observe_actual_fds_without_execution_registration() {
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let ancestor = harness.root("ancestor", false);
    let subject = harness.root("sibling", false);
    std::fs::create_dir(ancestor.path.join("nested")).unwrap();
    let ancestor_file = File::open(&ancestor.path).unwrap();
    let subject_file = File::open(&subject.path).unwrap();
    let nested_file = File::open(ancestor.path.join("nested")).unwrap();
    for (file, expected) in [
        (
            &ancestor_file,
            DirectoryRelationOutcome::Contains(ContainmentKind::SameDirectory),
        ),
        (
            &nested_file,
            DirectoryRelationOutcome::Contains(ContainmentKind::StrictDescendant),
        ),
        (&subject_file, DirectoryRelationOutcome::Disjoint),
    ] {
        let pair = qualify_directory_pair(&ancestor_file, file).unwrap();
        let facts = pair.facts().clone();
        let observation = client
            .observe_transferred_directories(pair)
            .await
            .unwrap_or_else(|error| panic!("observation-only held FD exchange failed: {error}"));
        assert_eq!(observation.outcome(), &expected);
        assert_eq!(observation.request().facts, facts);
        observation.revalidate_objects().unwrap();
    }
    assert!(
        client.registrations().is_empty(),
        "observation minted an execution registration"
    );
    assert!(
        !std::fs::read_dir(harness.journal()).unwrap().any(|entry| {
            entry.unwrap().path().extension().is_some_and(|extension| {
                ["registration", "execution", "retired"]
                    .iter()
                    .any(|forbidden| extension == *forbidden)
            })
        }),
        "observation changed durable execution/registration state"
    );
}

async fn raw_connection(harness: &Harness) -> (tokio::net::UnixStream, ExecutorBootId) {
    use calm_session::control::{ControlMsg, ControlReply};
    use calm_session::{read_frame, write_frame};
    let mut stream = tokio::net::UnixStream::connect(&harness.socket)
        .await
        .unwrap();
    write_frame(
        &mut stream,
        &ControlMsg::Execution(Box::new(ExecutionRequest::Handshake(ExecutionHandshake {
            required_contract: EXECUTION_ENVELOPE_VERSION,
            client_secret: harness.secret.clone(),
            mode: ExecutionProtocolMode::RegisteredV3,
        }))),
    )
    .await
    .unwrap();
    let ControlReply::Execution(reply) = read_frame::<ControlReply, _>(&mut stream).await.unwrap()
    else {
        panic!("not a typed handshake")
    };
    let ExecutionReply::Handshake {
        executor_boot,
        mode: ExecutionProtocolMode::RegisteredV3,
        ..
    } = *reply
    else {
        panic!("not registered")
    };
    (stream, executor_boot)
}

#[tokio::test]
async fn execution_transferred_directories_reject_wrong_count_kind_role_boot_and_facts() {
    use calm_session::control::{ControlMsg, ControlReply};
    use std::os::fd::AsFd;
    let harness = Harness::start().await;
    let ancestor = File::open(harness.root("ancestor", false).path).unwrap();
    let subject = File::open(harness.root("subject", false).path).unwrap();
    let regular = tempfile::tempfile().unwrap();
    let original = qualify_directory_pair(&ancestor, &subject).unwrap();
    for fault in 0..7 {
        let (stream, boot) = raw_connection(&harness).await;
        let mut request = TransferredDirectoryRelationRequest {
            executor_boot: boot,
            nonce: TransferNonce::fresh().unwrap(),
            facts: original.facts().clone(),
        };
        let descriptors = match fault {
            0 => vec![],
            1 => vec![ancestor.as_fd(), subject.as_fd(), ancestor.as_fd()],
            2 => vec![ancestor.as_fd(), regular.as_fd()],
            6 => vec![subject.as_fd(), ancestor.as_fd()],
            _ => vec![ancestor.as_fd(), subject.as_fd()],
        };
        match fault {
            3 => {
                request.facts.ancestor.role =
                    workspace::execution::objects::ObjectRole::GitDirectory
            }
            4 => request.executor_boot = ExecutorBootId::fresh().unwrap(),
            5 => request.facts.subject.inode += 1,
            _ => {}
        }
        rights::send(
            &stream,
            &ControlMsg::Execution(Box::new(ExecutionRequest::ObserveTransferredDirectories(
                Box::new(request),
            ))),
            &descriptors,
        )
        .await
        .unwrap();
        let (reply, received) = rights::receive::<ControlReply>(&stream).await.unwrap();
        assert!(received.is_empty());
        assert!(
            matches!(reply, ControlReply::Execution(reply) if matches!(*reply, ExecutionReply::Refused { .. } | ExecutionReply::ObjectQualificationFailed { .. })),
            "invalid transferred pair produced an observation"
        );
    }
}

#[tokio::test]
async fn execution_transferred_directories_observe_retained_objects_after_actual_rename_and_delete()
{
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let ancestor_path = harness.root("ancestor", false).path;
    let subject_path = harness.root("subject", false).path;
    let ancestor = File::open(&ancestor_path).unwrap();
    let subject = File::open(&subject_path).unwrap();
    let before = client
        .observe_transferred_directories(qualify_directory_pair(&ancestor, &subject).unwrap())
        .await
        .unwrap_or_else(|error| panic!("original pair: {error}"));
    assert_eq!(before.outcome(), &DirectoryRelationOutcome::Disjoint);
    std::fs::rename(&subject_path, ancestor_path.join("moved")).unwrap();
    std::fs::create_dir(&subject_path).unwrap();
    // Object continuity alone deliberately does not renew an old ancestry observation.
    before.revalidate_objects().unwrap();
    let after = client
        .observe_transferred_directories(qualify_directory_pair(&ancestor, &subject).unwrap())
        .await
        .unwrap_or_else(|error| panic!("retained pair: {error}"));
    assert_eq!(
        after.outcome(),
        &DirectoryRelationOutcome::Contains(ContainmentKind::StrictDescendant)
    );
    let replacement = File::open(&subject_path).unwrap();
    let replacement_result = client
        .observe_transferred_directories(qualify_directory_pair(&ancestor, &replacement).unwrap())
        .await
        .unwrap_or_else(|error| panic!("replacement pair: {error}"));
    assert_eq!(
        replacement_result.outcome(),
        &DirectoryRelationOutcome::Disjoint
    );
    std::fs::remove_dir(ancestor_path.join("moved")).unwrap();
    assert!(after.revalidate_objects().is_err());
    assert!(qualify_directory_pair(&ancestor, &subject).is_err());
}

#[tokio::test]
async fn execution_transferred_directories_server_qualifies_actual_unregistered_fds() {
    use calm_session::control::{ControlMsg, ControlReply};
    use std::os::fd::AsFd;
    use workspace::execution::objects::{
        DirectoryPairFacts, HeldSourceObjects, ObjectQualificationFailure,
        ObjectQualificationUnsupported,
    };
    let harness = Harness::start().await;
    let tmpfs = tempfile::tempdir_in("/dev/shm").unwrap();
    let held = HeldSourceObjects::open(tmpfs.path(), false).unwrap();
    let (stream, boot) = raw_connection(&harness).await;
    let request = TransferredDirectoryRelationRequest {
        executor_boot: boot,
        nonce: TransferNonce::fresh().unwrap(),
        facts: DirectoryPairFacts {
            mount_namespace: held.shape().mount_namespace,
            ancestor: held.shape().objects[0].clone(),
            subject: held.shape().objects[0].clone(),
        },
    };
    rights::send(
        &stream,
        &ControlMsg::Execution(Box::new(ExecutionRequest::ObserveTransferredDirectories(
            Box::new(request),
        ))),
        &[held.root().as_fd(), held.root().as_fd()],
    )
    .await
    .unwrap();
    let (reply, files) = rights::receive::<ControlReply>(&stream).await.unwrap();
    assert!(files.is_empty());
    assert!(
        matches!(reply, ControlReply::Execution(reply) if matches!(*reply, ExecutionReply::ObjectQualificationFailed { failure: ObjectQualificationFailure::Unsupported(ObjectQualificationUnsupported::Filesystem { magic: 0x01021994, .. }) })),
        "bare received facts bypassed actual server filesystem qualification"
    );
}

#[tokio::test]
async fn execution_transferred_directories_timeout_and_failure_keep_exact_native_ownership() {
    use std::{path::Path, time::Duration};
    use workspace::execution::objects::DirectoryRelationUnknown;
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
        let (shutdown, receive) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(calm_proc_supervisor::serve_with_listener(
            listener,
            socket.clone(),
            registry,
            receive,
        ));
        let directory = File::open(temp.path()).unwrap();
        let mut client = RegisteredExecutorConnection::connect(&socket, secret)
            .await
            .unwrap();
        let started = tokio::time::Instant::now();
        let observation = client
            .observe_transferred_directories(
                qualify_directory_pair(&directory, &directory).unwrap(),
            )
            .await
            .unwrap_or_else(|error| {
                panic!("controlled helper failure did not return its typed uncertainty: {error}")
            });
        assert_eq!(
            observation.outcome(),
            &DirectoryRelationOutcome::Unknown(expected)
        );
        assert!(started.elapsed() < Duration::from_secs(8));
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // This controlled stall/exit completed exact collection; outer RPC timeout alone would not.
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
        assert!(client.registrations().is_empty());
        let _ = shutdown.send(());
        server.await.unwrap().unwrap();
    }
}
