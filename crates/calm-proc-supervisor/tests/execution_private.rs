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
    // Export uses the authoritative native lease lookup, so it proves release before Retire.
    assert!(
        client
            .export(ExportRegistration {
                executor_boot: client.executor_boot().clone(),
                registration: request.identity.source_registration.clone(),
                nonce: TransferNonce::fresh().unwrap()
            })
            .await
            .is_err()
    );
    let mut client = harness.connect().await;
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
    let mut client = harness.connect().await;
    let root = harness.root("replay-root", false);
    let path = root.path.clone();
    let (request, held) = private_request(&client, root);
    assert!(matches!(
        client
            .prepare_private(request.clone(), &held)
            .await
            .unwrap(),
        ExecutionReply::Prepared
    ));
    drop(held);
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
