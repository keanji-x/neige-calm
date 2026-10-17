use super::*;
use crate::{
    control::{ControlMsg, ControlReply},
    read_frame, write_frame,
};
use std::{fs::File, os::fd::AsFd};
use tokio::{
    io::AsyncWriteExt,
    net::{UnixListener, UnixStream},
};
use workspace::execution::objects::{ObjectRole, qualify_directory_pair};

async fn negotiate(stream: &mut UnixStream) {
    let hello = read_frame::<ControlMsg, _>(stream).await.unwrap();
    assert!(
        matches!(hello, ControlMsg::Execution(request) if matches!(*request, ExecutionRequest::Handshake(_)))
    );
    write_frame(
        stream,
        &ControlReply::Execution(Box::new(ExecutionReply::Handshake {
            envelope_version: EXECUTION_ENVELOPE_VERSION,
            executor_boot: ExecutorBootId::fresh().unwrap(),
            mode: ExecutionProtocolMode::RegisteredV3,
        })),
    )
    .await
    .unwrap();
}

async fn receive_pair(stream: &UnixStream) -> TransferredDirectoryRelationRequest {
    let (message, files) = rights::receive::<ControlMsg>(stream).await.unwrap();
    let ControlMsg::Execution(request) = message else {
        panic!("not an execution envelope")
    };
    let ExecutionRequest::ObserveTransferredDirectories(request) = *request else {
        panic!("not a one-shot observation")
    };
    let files: [File; 2] = files
        .into_iter()
        .map(File::from)
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    let qualified = qualify_directory_pair(&files[0], &files[1]).unwrap();
    assert_eq!(
        *qualified.facts(),
        request.facts,
        "client sent scalar facts without actual matching FDs"
    );
    *request
}

#[tokio::test]
async fn execution_transferred_client_provenance_failure_has_no_witness_and_poisons_stream() {
    for fault in 0..5 {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("peer.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let ancestor = File::open(temp.path()).unwrap();
        let subject = ancestor.try_clone().unwrap();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            negotiate(&mut stream).await;
            let mut request = receive_pair(&stream).await;
            match fault {
                0 => request.nonce = TransferNonce::fresh().unwrap(),
                1 => request.executor_boot = ExecutorBootId::fresh().unwrap(),
                2 => request.facts.mount_namespace += 1,
                3 => request.facts.subject.inode += 1,
                4 => request.facts.ancestor.role = ObjectRole::GitDirectory,
                _ => unreachable!(),
            }
            rights::send(
                &stream,
                &ControlReply::Execution(Box::new(ExecutionReply::TransferredDirectoryRelation(
                    Box::new(TransferredDirectoryRelationReport {
                        request,
                        outcome: DirectoryRelationOutcome::Disjoint,
                    }),
                ))),
                &[],
            )
            .await
            .unwrap();
        });
        let mut client =
            RegisteredExecutorConnection::connect(&socket, CapabilityToken::fresh().unwrap())
                .await
                .unwrap();
        let outcome = client
            .observe_transferred_directories(qualify_directory_pair(&ancestor, &subject).unwrap())
            .await;
        assert!(outcome.is_err());
        assert!(
            client.in_flight,
            "invalid provenance left the stream reusable"
        );
        assert!(
            client
                .observe_transferred_directories(
                    qualify_directory_pair(&ancestor, &subject).unwrap()
                )
                .await
                .is_err()
        );
        peer.await.unwrap();
    }
}

#[tokio::test]
async fn execution_transferred_client_rejects_reply_rights_truncation_and_lost_reply() {
    for fault in 0..3 {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("peer.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let ancestor = File::open(temp.path()).unwrap();
        let subject = ancestor.try_clone().unwrap();
        let returned = ancestor.try_clone().unwrap();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            negotiate(&mut stream).await;
            let request = receive_pair(&stream).await;
            let reply =
                ControlReply::Execution(Box::new(ExecutionReply::TransferredDirectoryRelation(
                    Box::new(TransferredDirectoryRelationReport {
                        request,
                        outcome: DirectoryRelationOutcome::Disjoint,
                    }),
                )));
            match fault {
                0 => rights::send(&stream, &reply, &[returned.as_fd()])
                    .await
                    .unwrap(),
                1 => {
                    let mut bytes = Vec::new();
                    write_frame(&mut bytes, &reply).await.unwrap();
                    stream.write_all(&bytes[..bytes.len() / 2]).await.unwrap();
                }
                2 => {}
                _ => unreachable!(),
            }
        });
        let mut client =
            RegisteredExecutorConnection::connect(&socket, CapabilityToken::fresh().unwrap())
                .await
                .unwrap();
        assert!(
            client
                .observe_transferred_directories(
                    qualify_directory_pair(&ancestor, &subject).unwrap()
                )
                .await
                .is_err()
        );
        assert!(
            client.in_flight,
            "an interrupted or descriptor-bearing reply left a reusable stream"
        );
        peer.await.unwrap();
    }
}

#[tokio::test]
async fn execution_transferred_client_typed_qualification_failure_does_not_mint_a_witness() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("peer.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let ancestor = File::open(temp.path()).unwrap();
    let subject = ancestor.try_clone().unwrap();
    let peer = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        negotiate(&mut stream).await;
        receive_pair(&stream).await;
        rights::send(
            &stream,
            &ControlReply::Execution(Box::new(ExecutionReply::ObjectQualificationFailed {
                failure: ObjectQualificationFailure::Unsupported(
                    workspace::execution::objects::ObjectQualificationUnsupported::Platform,
                ),
            })),
            &[],
        )
        .await
        .unwrap();
    });
    let mut client =
        RegisteredExecutorConnection::connect(&socket, CapabilityToken::fresh().unwrap())
            .await
            .unwrap();
    let result = client
        .observe_transferred_directories(qualify_directory_pair(&ancestor, &subject).unwrap())
        .await;
    assert!(matches!(
        result,
        Err(TransferredDirectoryObservationError::Qualification(
            ObjectQualificationFailure::Unsupported(_)
        ))
    ));
    assert!(
        !client.in_flight,
        "a complete typed refusal should not poison the transport"
    );
    peer.await.unwrap();
}
