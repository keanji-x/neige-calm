use super::*;
use crate::{
    control::{ControlMsg, ControlReply},
    read_frame, write_frame,
};
use std::os::{fd::AsFd, unix::fs::MetadataExt};
use tokio::{io::AsyncWriteExt, net::UnixListener};
use workspace::execution::objects::{ContainmentKind, HeldSourceObjects};

fn request(boot: ExecutorBootId, root: &std::path::Path) -> DirectoryRelationRequest {
    let stat = std::fs::metadata(root).unwrap();
    let held = HeldSourceObjects::open(root, false).unwrap();
    let id = ObjectRegistrationId::fresh().unwrap();
    // This is a wire-peer fixture, not an executor registration/admission proof.
    let receipt = ObjectRegistrationReceipt {
        contract: PERSISTED_RECEIPT_CONTRACT_VERSION,
        id: id.clone(),
        transfer: AttachRegistration {
            registration_id: id,
            executor_boot: boot.clone(),
            nonce: TransferNonce::fresh().unwrap(),
            incarnation: SourceObjectIncarnation::ExternalDir(
                ExternalDirectoryIncarnationId::fresh().unwrap(),
            ),
            registration: ExecutionRegistration::ExternalDir {
                directory_id: ExternalDirectoryId::fresh().unwrap(),
                root: DirectoryRegistration {
                    path: root.into(),
                    device: stat.dev(),
                    inode: stat.ino(),
                },
                registration_version: 9,
                admission_open: true,
                authority: ExecutionAuthority {
                    grant_id: ExecutionGrantId::fresh().unwrap(),
                    authorization_binding: AuthorizationBindingId::fresh().unwrap(),
                    authorization_version: 3,
                },
            },
            shape: held.shape().clone(),
        },
    };
    DirectoryRelationRequest {
        executor_boot: boot,
        nonce: TransferNonce::fresh().unwrap(),
        ancestor: RegisteredDirectoryRoot {
            receipt: receipt.clone(),
            role: ObjectRole::Root,
        },
        subject: RegisteredDirectoryRoot {
            receipt,
            role: ObjectRole::Root,
        },
    }
}

async fn negotiate(stream: &mut tokio::net::UnixStream, boot: ExecutorBootId) {
    assert!(matches!(
        read_frame::<ControlMsg, _>(stream).await.unwrap(),
        ControlMsg::Execution(request) if matches!(request.as_ref(), ExecutionRequest::Handshake(_))
    ));
    write_frame(
        stream,
        &ControlReply::Execution(Box::new(ExecutionReply::Handshake {
            envelope_version: EXECUTION_ENVELOPE_VERSION,
            executor_boot: boot,
            mode: ExecutionProtocolMode::RegisteredV3,
        })),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn execution_directory_observation_wrong_provenance_poisoned_connection_has_no_witness() {
    for fault in 0..4 {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("peer.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let boot = ExecutorBootId::fresh().unwrap();
        let query = request(boot.clone(), temp.path());
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            negotiate(&mut stream, boot).await;
            let (message, rights) = rights::receive::<ControlMsg>(&stream).await.unwrap();
            assert!(rights.is_empty());
            let ControlMsg::Execution(message) = message else {
                panic!("not an execution request");
            };
            let ExecutionRequest::ObserveDirectories(mut query) = *message else {
                panic!("not a directory observation");
            };
            match fault {
                0 => query.nonce = TransferNonce::fresh().unwrap(),
                1 => query.executor_boot = ExecutorBootId::fresh().unwrap(),
                2 => query.subject.receipt.id = ObjectRegistrationId::fresh().unwrap(),
                3 => query.subject.role = ObjectRole::GitDirectory,
                _ => unreachable!(),
            }
            rights::send(
                &stream,
                &ControlReply::Execution(Box::new(ExecutionReply::DirectoryRelation(Box::new(
                    DirectoryRelationReport {
                        request: *query,
                        outcome: DirectoryRelationOutcome::Contains(ContainmentKind::SameDirectory),
                    },
                )))),
                &[],
            )
            .await
            .unwrap();
        });
        let mut client =
            RegisteredExecutorConnection::connect(&socket, CapabilityToken::fresh().unwrap())
                .await
                .unwrap();
        assert!(client.observe_directories(query.clone()).await.is_err());
        assert!(
            client.observe_directories(query).await.is_err(),
            "an invalid reply left a reusable stream"
        );
        peer.await.unwrap();
    }
}

#[tokio::test]
async fn execution_directory_observation_rejects_reply_rights_truncation_and_lost_reply() {
    for fault in 0..3 {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("peer.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let boot = ExecutorBootId::fresh().unwrap();
        let query = request(boot.clone(), temp.path());
        let file = std::fs::File::open(temp.path()).unwrap();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            negotiate(&mut stream, boot).await;
            let (message, descriptors) = rights::receive::<ControlMsg>(&stream).await.unwrap();
            assert!(descriptors.is_empty());
            let ControlMsg::Execution(message) = message else {
                panic!("not an execution request");
            };
            let ExecutionRequest::ObserveDirectories(query) = *message else {
                panic!("not an observation");
            };
            let reply = ControlReply::Execution(Box::new(ExecutionReply::DirectoryRelation(
                Box::new(DirectoryRelationReport {
                    request: *query,
                    outcome: DirectoryRelationOutcome::Disjoint,
                }),
            )));
            match fault {
                0 => rights::send(&stream, &reply, &[file.as_fd()])
                    .await
                    .unwrap(),
                1 => {
                    let mut frame = Vec::new();
                    write_frame(&mut frame, &reply).await.unwrap();
                    stream.write_all(&frame[..frame.len() / 2]).await.unwrap();
                }
                2 => {}
                _ => unreachable!(),
            }
        });
        let mut client =
            RegisteredExecutorConnection::connect(&socket, CapabilityToken::fresh().unwrap())
                .await
                .unwrap();
        assert!(client.observe_directories(query.clone()).await.is_err());
        assert!(client.observe_directories(query).await.is_err());
        peer.await.unwrap();
    }
}
