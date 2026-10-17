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
