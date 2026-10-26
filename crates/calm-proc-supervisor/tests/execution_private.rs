use super::execution_support::*;
use calm_session::control::{IoMode, execution::*};
use workspace::{execution::objects::HeldSourceObjects, identity::WorkspaceCwd};

fn private_request(
    client: &RegisteredExecutorConnection,
    root: DirectoryRegistration,
) -> (PrepareExecution, HeldSourceObjects) {
    let held = HeldSourceObjects::open(&root.path, false).unwrap();
    let authority = authority();
    let directory_id = ExternalDirectoryId::fresh().unwrap();
    let registration_id = ObjectRegistrationId::fresh().unwrap();
    let incarnation =
        SourceObjectIncarnation::ExternalDir(ExternalDirectoryIncarnationId::fresh().unwrap());
    let transfer = AttachRegistration {
        registration_id: registration_id.clone(),
        executor_boot: client.executor_boot().clone(),
        nonce: TransferNonce::fresh().unwrap(),
        incarnation: incarnation.clone(),
        shape: held.shape().clone(),
        registration: ExecutionRegistration::ExternalDir {
            directory_id: directory_id.clone(),
            root,
            registration_version: 1,
            admission_open: true,
            authority: authority.clone(),
        },
    };
    let request = PrepareExecution {
        identity: ExecutionIdentity {
            execution_id: ExecutionId::fresh().unwrap(),
            executor_boot: client.executor_boot().clone(),
            ticket_id: LaunchTicketId::fresh().unwrap(),
            source_registration: registration_id,
            source_incarnation: incarnation,
            authority,
            scope: ExecutionScope::ExternalDir {
                directory_id,
                registration_version: 1,
                cwd: WorkspaceCwd::root(),
            },
            operation_id: "private-wire".into(),
            specification_id: SpecificationId::fresh().unwrap(),
            process_alias: ExecutionId::fresh().unwrap().as_str().into(),
        },
        source: PreparedSource::Private(Box::new(transfer)),
        ticket: CapabilityToken::fresh().unwrap(),
        control_tokens: ExecutionControlTokens::fresh().unwrap(),
        handoff: None,
        specification: ExecutionSpecification {
            ambient_environment: calm_session::control::execution::AmbientEnvironment::None,
            program: "/bin/true".into(),
            args: vec![],
            environment: vec![],
            io_mode: IoMode::Pipe,
            replay_bytes: 1024,
        },
    };
    (request, held)
}

#[tokio::test]
async fn private_prepare_wire_watch_reconnect_and_retire() {
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let mut watcher = harness.connect().await.watch().await.unwrap();
    assert_eq!(watcher.boot(), client.executor_boot());
    assert_eq!(watcher.sequence(), 0);
    let root = harness.root("private-wire", false);
    let (request, held) = private_request(&client, root);
    assert!(matches!(
        client
            .prepare_private(request.clone(), &held)
            .await
            .unwrap(),
        ExecutionReply::Prepared
    ));
    assert!(matches!(
        client
            .request(ExecutionRequest::Retire {
                execution_id: request.identity.execution_id.clone(),
                lifecycle: request.control_tokens.lifecycle.clone(),
            })
            .await
            .unwrap(),
        ExecutionReply::Busy(BusyReason::OutputOpen)
    ));
    assert!(matches!(
        client
            .request(ExecutionRequest::Retire {
                execution_id: request.identity.execution_id.clone(),
                lifecycle: CapabilityToken::fresh().unwrap(),
            })
            .await
            .unwrap(),
        ExecutionReply::Refused { .. }
    ));
    drop(held);
    drop(client); // Executor retains the private lease across a kernel connection restart.
    let mut client = harness.connect().await;
    assert!(matches!(
        client
            .request(ExecutionRequest::Inspect(
                request.identity.execution_id.clone()
            ))
            .await
            .unwrap(),
        ExecutionReply::Inspected {
            state: ExecutionState::Prepared,
            ..
        }
    ));
    let launched = client
        .request(ExecutionRequest::Launch(consume(&request)))
        .await
        .unwrap();
    assert!(
        matches!(
            launched,
            ExecutionReply::Receipt {
                state: ExecutionState::Spawned,
                ..
            }
        ),
        "{launched:?}"
    );
    for expected in 1..=7 {
        let change = tokio::time::timeout(std::time::Duration::from_secs(5), watcher.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(change, (expected, request.identity.execution_id.clone()));
    }
    assert!(matches!(
        client
            .request(ExecutionRequest::Inspect(
                request.identity.execution_id.clone()
            ))
            .await
            .unwrap(),
        ExecutionReply::Inspected {
            state: ExecutionState::Drained { .. },
            output_end: OutputEnd::Complete { .. },
            ..
        }
    ));
    let PreparedSource::Private(transfer) = &request.source else {
        unreachable!()
    };
    let ExecutionRegistration::ExternalDir { root, .. } = &transfer.registration else {
        unreachable!()
    };
    assert_root_released(&harness, root);
    let reply = client
        .request(ExecutionRequest::Retire {
            execution_id: request.identity.execution_id.clone(),
            lifecycle: request.control_tokens.lifecycle.clone(),
        })
        .await
        .unwrap();
    assert!(
        matches!(
            reply,
            ExecutionReply::Retired(ExecutionTombstone {
                outcome: TombstoneOutcome::Retired,
                ..
            })
        ),
        "{reply:?}"
    );
    assert_eq!(
        watcher.changed().await.unwrap(),
        (8, request.identity.execution_id.clone())
    );
    drop(watcher);
    let watcher = harness.connect().await.watch().await.unwrap();
    assert_eq!(watcher.sequence(), 8);
    assert!(matches!(
        client
            .request(ExecutionRequest::Inspect(
                request.identity.execution_id.clone()
            ))
            .await
            .unwrap(),
        ExecutionReply::Retired(_)
    ));
    assert!(
        !harness
            .journal()
            .join(format!("{}.output", request.identity.execution_id.as_str()))
            .exists()
    );
}

#[tokio::test]
async fn private_prepare_wire_exact_replay_ignores_replaced_root_and_boot() {
    let mut harness = Harness::start().await;
    let client = harness.connect().await;
    let root = harness.root("replay-root", false);
    let path = root.path.clone();
    let (request, held) = private_request(&client, root);
    use std::os::fd::AsRawFd;
    assert_eq!(
        unsafe { libc::flock(held.root().as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let fresh = HeldSourceObjects::open(&path, false).unwrap();
    let mut watcher = harness.connect().await.watch().await.unwrap();
    let mut socket = tokio::net::UnixStream::connect(&harness.socket)
        .await
        .unwrap();
    calm_session::write_frame(
        &mut socket,
        &calm_session::control::ControlMsg::Execution(Box::new(ExecutionRequest::Handshake(
            ExecutionHandshake {
                required_contract: EXECUTION_ENVELOPE_VERSION,
                client_secret: harness.secret.clone(),
                mode: ExecutionProtocolMode::RegisteredV3,
            },
        ))),
    )
    .await
    .unwrap();
    assert!(matches!(
        calm_session::read_frame::<calm_session::control::ControlReply, _>(&mut socket).await.unwrap(),
        calm_session::control::ControlReply::Execution(reply)
            if matches!(*reply, ExecutionReply::Handshake { .. })
    ));
    rights::send(
        &socket,
        &calm_session::control::ControlMsg::Execution(Box::new(ExecutionRequest::Prepare(
            request.clone(),
        ))),
        &held.descriptors(),
    )
    .await
    .unwrap();
    drop(socket); // Disconnect without reading the Prepare ACK.
    drop(held);
    drop(client);
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(5), watcher.changed())
            .await
            .unwrap()
            .unwrap(),
        (1, request.identity.execution_id.clone())
    );
    drop(watcher);
    let journal_path = harness.journal().join(format!(
        "{}.execution",
        request.identity.execution_id.as_str()
    ));
    let durable = std::fs::read(&journal_path).unwrap();
    let mut client = harness.connect().await;
    // Lost ACK replay must retain the original open description, not the retry's FD.
    let replay = client
        .prepare_private(request.clone(), &fresh)
        .await
        .unwrap();
    assert!(
        matches!(
            replay,
            ExecutionReply::Receipt {
                state: ExecutionState::Prepared,
                receipt: None
            }
        ),
        "{replay:?}"
    );
    assert_eq!(std::fs::read(&journal_path).unwrap(), durable);
    assert_eq!(
        serde_json::to_value(
            client
                .prepare_private(request.clone(), &fresh)
                .await
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(&replay).unwrap()
    );
    assert_eq!(std::fs::read(&journal_path).unwrap(), durable);
    assert_eq!(
        unsafe { libc::flock(fresh.root().as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().kind(),
        std::io::ErrorKind::WouldBlock
    );
    std::fs::rename(&path, harness.temp.path().join("original-root")).unwrap();
    std::fs::create_dir(&path).unwrap();
    let replacement = HeldSourceObjects::open(&path, false).unwrap();
    assert!(matches!(
        client
            .prepare_private(request.clone(), &replacement)
            .await
            .unwrap(),
        ExecutionReply::Receipt {
            state: ExecutionState::Prepared,
            ..
        }
    ));
    assert!(matches!(
        client
            .request(ExecutionRequest::Launch(consume(&request)))
            .await
            .unwrap(),
        ExecutionReply::Receipt {
            state: ExecutionState::NoChild { .. },
            ..
        }
    ));
    assert_eq!(
        unsafe { libc::flock(fresh.root().as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    drop(client);
    // Terminal exact retries are independent of the executor boot, just like Inspect.
    unsafe {
        libc::kill(harness.child.id() as i32, libc::SIGTERM);
    }
    harness.child.wait().unwrap();
    harness.child = Harness::spawn(harness.temp.path(), &harness.socket);
    let mut client = harness.connect().await;
    assert_ne!(client.executor_boot(), &request.identity.executor_boot);
    assert!(matches!(
        client
            .prepare_private(request.clone(), &replacement)
            .await
            .unwrap(),
        ExecutionReply::Receipt {
            state: ExecutionState::NoChild { .. },
            ..
        }
    ));
}

#[tokio::test]
async fn legacy_handshake_server_wire_stable_and_typed_v3_refused() {
    use calm_session::{
        control::{ControlMsg, ControlReply},
        read_frame, write_frame,
    };
    use tokio::io::AsyncReadExt;
    let harness = Harness::start().await;
    let mut socket = tokio::net::UnixStream::connect(&harness.socket)
        .await
        .unwrap();
    write_frame(
        &mut socket,
        &ControlMsg::Execution(Box::new(ExecutionRequest::Handshake(ExecutionHandshake {
            required_contract: 0,
            client_secret: CapabilityToken::fresh().unwrap(),
            mode: ExecutionProtocolMode::LegacyWorktree,
        }))),
    )
    .await
    .unwrap();
    let mut header = [0; 10];
    socket.read_exact(&mut header).await.unwrap();
    assert_eq!(&header[..6], b"NEIG\x00\x04");
    let mut bytes = vec![0; u32::from_be_bytes(header[6..10].try_into().unwrap()) as usize];
    socket.read_exact(&mut bytes).await.unwrap();
    assert_eq!(bytes.len(), 37);
    assert_eq!(&bytes[..4], &[14, 0, 0, 32]); // released reply tags and envelope=0
    assert!(bytes[4..36].iter().all(u8::is_ascii_hexdigit));
    assert_eq!(bytes[36], 1); // LegacyWorktree
    // The production client still negotiates Legacy, and the server rejects old typed clients.
    connect_legacy(&harness.socket).await.unwrap();
    write_frame(
        &mut socket,
        &ControlMsg::Execution(Box::new(ExecutionRequest::Handshake(ExecutionHandshake {
            required_contract: 3,
            client_secret: harness.secret.clone(),
            mode: ExecutionProtocolMode::RegisteredV3,
        }))),
    )
    .await
    .unwrap();
    assert!(
        matches!(read_frame::<ControlReply, _>(&mut socket).await.unwrap(),
        ControlReply::Execution(r) if matches!(*r, ExecutionReply::Refused { .. }))
    );
}

#[path = "execution_private/c0_compat.rs"]
mod c0_compat;

#[path = "execution_private/qualification.rs"]
mod qualification;

#[tokio::test]
async fn private_retirement_preserves_peer_and_does_not_grow_fds() {
    let h = Harness::start().await;
    let mut c = h.connect().await;
    let root = h.root("retirement", false);
    let (peer, held) = private_request(&c, root.clone());
    assert!(matches!(
        c.prepare_private(peer.clone(), &held).await.unwrap(),
        ExecutionReply::Prepared
    ));
    let fd_count = || {
        std::fs::read_dir(format!("/proc/{}/fd", h.child.id()))
            .unwrap()
            .count()
    };
    let baseline = fd_count();
    for _ in 0..24 {
        let (r, held) = private_request(&c, root.clone());
        assert!(matches!(
            c.prepare_private(r.clone(), &held).await.unwrap(),
            ExecutionReply::Prepared
        ));
        assert!(matches!(
            c.request(ExecutionRequest::Revoke(consume(&r)))
                .await
                .unwrap(),
            ExecutionReply::RevocationAcknowledged {
                state: ExecutionState::Revoked,
                ..
            }
        ));
        let retire = ExecutionRequest::Retire {
            execution_id: r.identity.execution_id.clone(),
            lifecycle: r.control_tokens.lifecycle.clone(),
        };
        let tombstone = c.request(retire.clone()).await.unwrap();
        assert!(
            matches!(&tombstone, ExecutionReply::Retired(t) if t.registration.as_ref() == Some(&r.identity.source_registration))
        );
        assert!(
            matches!((c.request(retire).await.unwrap(), tombstone), (ExecutionReply::Retired(a), ExecutionReply::Retired(b)) if a == b)
        );
        assert!(matches!(
            c.request(ExecutionRequest::Inspect(
                peer.identity.execution_id.clone()
            ))
            .await
            .unwrap(),
            ExecutionReply::Inspected {
                state: ExecutionState::Prepared,
                ..
            }
        ));
        assert_eq!(fd_count(), baseline);
    }
    drop(held); // Only the supervisor's original private lease can satisfy Launch.
    let launched = c
        .request(ExecutionRequest::Launch(consume(&peer)))
        .await
        .unwrap();
    assert!(
        matches!(
            launched,
            ExecutionReply::Receipt {
                state: ExecutionState::Spawned,
                receipt: Some(_)
            }
        ),
        "{launched:?}"
    );
}

#[tokio::test]
async fn recovery_3192_supervisor_refuses_released_v10_kernel() {
    use calm_session::{
        control::{ControlMsg, ControlReply},
        read_frame, write_frame,
    };
    let harness = Harness::start().await;
    let mut stream = tokio::net::UnixStream::connect(&harness.socket)
        .await
        .unwrap();
    write_frame(
        &mut stream,
        &ControlMsg::Execution(Box::new(ExecutionRequest::Handshake(ExecutionHandshake {
            required_contract: 10,
            client_secret: harness.secret.clone(),
            mode: ExecutionProtocolMode::RegisteredV3,
        }))),
    )
    .await
    .unwrap();
    assert!(
        matches!(read_frame::<ControlReply, _>(&mut stream).await.unwrap(), ControlReply::Execution(reply) if matches!(*reply, ExecutionReply::Refused { .. }))
    );
}
