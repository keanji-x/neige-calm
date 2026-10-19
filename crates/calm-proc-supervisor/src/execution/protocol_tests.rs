use super::*;
use calm_session::control::IoMode;
use std::{
    os::{fd::AsFd, unix::fs::MetadataExt},
    time::Duration,
};
use workspace::{execution::objects::HeldSourceObjects, identity::WorkspaceCwd};

fn executor(temp: &tempfile::TempDir, secret: &CapabilityToken) -> Arc<Executor> {
    Executor::open(ExecutorConfig {
        journal_directory: temp.path().join("journal"),
        helper_program: std::fs::canonicalize("/bin/sh").unwrap(),
        kernel_client_secret: secret.clone(),
    })
    .unwrap()
}
pub(super) fn private(
    executor: &Executor,
    path: &std::path::Path,
) -> (PrepareExecution, HeldSourceObjects) {
    let held = HeldSourceObjects::open(path, false).unwrap();
    let stat = std::fs::metadata(path).unwrap();
    let authority = ExecutionAuthority {
        grant_id: ExecutionGrantId::fresh().unwrap(),
        authorization_binding: AuthorizationBindingId::fresh().unwrap(),
        authorization_version: 1,
    };
    let directory_id = ExternalDirectoryId::fresh().unwrap();
    let transfer = AttachRegistration {
        registration_id: ObjectRegistrationId::fresh().unwrap(),
        executor_boot: executor.boot.clone(),
        nonce: TransferNonce::fresh().unwrap(),
        incarnation: SourceObjectIncarnation::ExternalDir(
            ExternalDirectoryIncarnationId::fresh().unwrap(),
        ),
        registration: ExecutionRegistration::ExternalDir {
            directory_id: directory_id.clone(),
            root: DirectoryRegistration {
                path: path.to_owned(),
                device: stat.dev(),
                inode: stat.ino(),
            },
            registration_version: 1,
            admission_open: true,
            authority: authority.clone(),
        },
        shape: held.shape().clone(),
    };
    (
        PrepareExecution {
            identity: ExecutionIdentity {
                execution_id: ExecutionId::fresh().unwrap(),
                ticket_id: LaunchTicketId::fresh().unwrap(),
                executor_boot: executor.boot.clone(),
                source_registration: transfer.registration_id.clone(),
                source_incarnation: transfer.incarnation.clone(),
                authority,
                scope: ExecutionScope::ExternalDir {
                    directory_id,
                    registration_version: 1,
                    cwd: WorkspaceCwd::root(),
                },
                operation_id: "private-test".into(),
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
        },
        held,
    )
}
fn descriptors(held: &HeldSourceObjects) -> Vec<std::os::fd::OwnedFd> {
    held.descriptors()
        .iter()
        .map(|fd| fd.as_fd().try_clone_to_owned().unwrap())
        .collect()
}
fn consume(request: &PrepareExecution) -> ConsumeExecution {
    ConsumeExecution {
        execution_id: request.identity.execution_id.clone(),
        ticket_id: request.identity.ticket_id.clone(),
        ticket: request.ticket.clone(),
    }
}
async fn inspect(
    executor: &Arc<Executor>,
    secret: &CapabilityToken,
    request: &PrepareExecution,
) -> ExecutionReply {
    executor
        .dispatch(
            secret,
            ExecutionRequest::Inspect(request.identity.execution_id.clone()),
        )
        .await
}
pub(super) fn assert_released(executor: &Executor) {
    assert!(
        executor.live_objects.lock().unwrap().is_empty(),
        "private native lease leaked"
    );
    assert!(
        executor.registrations.lock().unwrap().is_empty(),
        "private lifecycle projection leaked"
    );
}
pub(super) async fn prepare(
    executor: &Executor,
    request: &PrepareExecution,
    held: &HeldSourceObjects,
) {
    assert!(matches!(
        executor
            .prepare_objects(request.clone(), descriptors(held))
            .await
            .unwrap(),
        ExecutionReply::Prepared
    ));
}

#[tokio::test]
async fn private_prepare_failure_after_staging_publishes_nothing() {
    for schedule in 0..3 {
        let temp = tempfile::tempdir().unwrap();
        let secret = CapabilityToken::fresh().unwrap();
        let executor = executor(&temp, &secret);
        let path = temp.path().join("source");
        std::fs::create_dir(&path).unwrap();
        let (request, held) = private(&executor, &path);
        let received = descriptors(&held);
        if schedule == 0 {
            std::fs::rename(&path, temp.path().join("old-source")).unwrap();
            std::fs::create_dir(&path).unwrap();
        } else {
            executor
                .store
                .fail_execution_write
                .store(schedule, std::sync::atomic::Ordering::SeqCst);
        }
        assert!(
            executor
                .prepare_objects(request.clone(), received)
                .await
                .is_err()
        );
        assert_released(&executor);
        assert!(matches!(
            inspect(&executor, &secret, &request).await,
            ExecutionReply::Absent
        ));
        let execution_file = executor.store.path().join(format!(
            "{}.execution",
            request.identity.execution_id.as_str()
        ));
        let leftover = std::fs::read(&execution_file).ok();
        assert_eq!(leftover.is_some(), schedule == 2);
        let reply = executor
            .dispatch(&secret, ExecutionRequest::Revoke(consume(&request)))
            .await;
        assert!(
            matches!(
                reply,
                ExecutionReply::Retired(ExecutionTombstone {
                    outcome: TombstoneOutcome::Revoked,
                    ..
                })
            ),
            "{reply:?}"
        );
        assert!(!execution_file.exists());
        assert_released(&executor);
        // Even malformed stale journals cannot override a tombstone at open.
        std::fs::write(
            &execution_file,
            leftover.unwrap_or_else(|| b"stale".to_vec()),
        )
        .unwrap();
        assert!(matches!(
            inspect(&executor, &secret, &request).await,
            ExecutionReply::Retired(_)
        ));
        assert!(
            executor
                .prepare_objects(request.clone(), descriptors(&held))
                .await
                .is_err()
        );
        drop(executor);
        let reopened = self::executor(&temp, &secret);
        assert!(matches!(
            inspect(&reopened, &secret, &request).await,
            ExecutionReply::Retired(_)
        ));
        assert!(
            reopened
                .prepare_objects(request.clone(), descriptors(&held))
                .await
                .is_err()
        );
        assert_released(&reopened);
    }
}

#[tokio::test]
async fn private_root_replaced_after_prepare_launch_no_child() {
    let temp = tempfile::tempdir().unwrap();
    let secret = CapabilityToken::fresh().unwrap();
    let executor = executor(&temp, &secret);
    let path = temp.path().join("source");
    std::fs::create_dir(&path).unwrap();
    let (request, held) = private(&executor, &path);
    prepare(&executor, &request, &held).await;
    std::fs::rename(&path, temp.path().join("old")).unwrap();
    std::fs::create_dir(&path).unwrap();
    let result = executor
        .dispatch(&secret, ExecutionRequest::Launch(consume(&request)))
        .await;
    assert!(
        matches!(
            result,
            ExecutionReply::Receipt {
                state: ExecutionState::NoChild { .. },
                ..
            }
        ),
        "{result:?}"
    );
    assert_released(&executor);
}

#[tokio::test]
async fn private_prepare_kernel_restart_executor_alive_releases_on_settle() {
    let temp = tempfile::tempdir().unwrap();
    let secret = CapabilityToken::fresh().unwrap();
    let executor = executor(&temp, &secret);
    let (request, held) = private(&executor, temp.path());
    prepare(&executor, &request, &held).await;
    drop(held); // kernel lease is gone; a new client reconciles the same frozen intent.
    assert_eq!(executor.live_objects.lock().unwrap().len(), 1);
    assert!(matches!(
        inspect(&executor, &secret, &request).await,
        ExecutionReply::Inspected {
            state: ExecutionState::Prepared,
            ..
        }
    ));
    assert!(matches!(
        executor
            .dispatch(&secret, ExecutionRequest::Revoke(consume(&request)))
            .await,
        ExecutionReply::RevocationAcknowledged {
            state: ExecutionState::Revoked,
            ..
        }
    ));
    assert_released(&executor);
}

#[tokio::test]
async fn private_prepare_executor_restart_releases() {
    let temp = tempfile::tempdir().unwrap();
    let secret = CapabilityToken::fresh().unwrap();
    let executor = executor(&temp, &secret);
    let (request, held) = private(&executor, temp.path());
    prepare(&executor, &request, &held).await;
    assert_eq!(
        std::fs::read_dir(executor.store.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "registration"))
            .count(),
        0
    );
    drop(executor);
    let executor = self::executor(&temp, &secret);
    assert!(matches!(
        inspect(&executor, &secret, &request).await,
        ExecutionReply::Inspected {
            state: ExecutionState::Revoked,
            ..
        }
    ));
    assert_released(&executor);
    assert!(matches!(
        executor
            .dispatch(&secret, ExecutionRequest::Launch(consume(&request)))
            .await,
        ExecutionReply::Receipt {
            state: ExecutionState::Revoked,
            ..
        }
    ));
}

#[tokio::test]
async fn private_prepare_exact_replay_keeps_original_description() {
    let temp = tempfile::tempdir().unwrap();
    let secret = CapabilityToken::fresh().unwrap();
    let executor = executor(&temp, &secret);
    let path = temp.path().join("source");
    std::fs::create_dir(&path).unwrap();
    let (request, held) = private(&executor, &path);
    prepare(&executor, &request, &held).await;
    let original = executor
        .live_objects
        .lock()
        .unwrap()
        .get(&request.identity.source_registration)
        .unwrap()
        .clone();
    std::fs::rename(&path, temp.path().join("old")).unwrap();
    std::fs::create_dir(&path).unwrap();
    let replacement = HeldSourceObjects::open(&path, false).unwrap();
    assert!(matches!(
        executor
            .prepare_objects(request.clone(), descriptors(&replacement))
            .await
            .unwrap(),
        ExecutionReply::Receipt {
            state: ExecutionState::Prepared,
            ..
        }
    ));
    assert!(Arc::ptr_eq(
        &original,
        executor
            .live_objects
            .lock()
            .unwrap()
            .get(&request.identity.source_registration)
            .unwrap()
    ));
    let mut changed = request.clone();
    changed.specification.args.push("different".into());
    assert!(
        executor
            .prepare_objects(changed, descriptors(&replacement))
            .await
            .is_err()
    );
    executor.revoke(consume(&request)).await.unwrap();
    assert_released(&executor);
}

#[tokio::test]
async fn private_attach_refuses_directory_of_other_execution() {
    let temp = tempfile::tempdir().unwrap();
    let secret = CapabilityToken::fresh().unwrap();
    let executor = executor(&temp, &secret);
    let (request, held) = private(&executor, temp.path());
    prepare(&executor, &request, &held).await;
    let mut other = request.clone();
    other.identity.execution_id = ExecutionId::fresh().unwrap();
    other.identity.ticket_id = LaunchTicketId::fresh().unwrap();
    assert!(
        executor
            .prepare_objects(other.clone(), descriptors(&held))
            .await
            .is_err()
    );
    executor.revoke(consume(&request)).await.unwrap();
    executor
        .retire_execution(
            request.identity.execution_id.clone(),
            request.control_tokens.lifecycle.clone(),
        )
        .await
        .unwrap();
    assert!(
        executor
            .prepare_objects(other, descriptors(&held))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn revoke_unknown_id_blocks_late_prepare() {
    let temp = tempfile::tempdir().unwrap();
    let secret = CapabilityToken::fresh().unwrap();
    let executor = executor(&temp, &secret);
    let (request, held) = private(&executor, temp.path());
    assert!(matches!(
        inspect(&executor, &secret, &request).await,
        ExecutionReply::Absent
    ));
    let reply = executor
        .dispatch(&secret, ExecutionRequest::Revoke(consume(&request)))
        .await;
    assert!(
        matches!(
            reply,
            ExecutionReply::Retired(ExecutionTombstone {
                outcome: TombstoneOutcome::Revoked,
                ..
            })
        ),
        "{reply:?}"
    );
    assert!(
        executor
            .prepare_objects(request.clone(), descriptors(&held))
            .await
            .is_err()
    );
    let mut wrong = consume(&request);
    wrong.ticket_id = LaunchTicketId::fresh().unwrap();
    assert!(matches!(
        executor
            .dispatch(&secret, ExecutionRequest::Revoke(wrong))
            .await,
        ExecutionReply::Refused { .. }
    ));
    assert_released(&executor);
}

#[tokio::test]
async fn changed_after_every_persisted_transition() {
    let temp = tempfile::tempdir().unwrap();
    let secret = CapabilityToken::fresh().unwrap();
    let executor = executor(&temp, &secret);
    let (watching, mut changes) = executor.watch().unwrap();
    assert!(
        matches!(watching, ExecutionReply::Watching { ref boot, seq: 0 } if *boot == executor.boot)
    );
    let (request, held) = private(&executor, temp.path());
    prepare(&executor, &request, &held).await;
    assert_eq!(
        changes.try_recv().unwrap(),
        (1, request.identity.execution_id.clone())
    );
    executor.revoke(consume(&request)).await.unwrap();
    assert_eq!(
        changes.try_recv().unwrap(),
        (2, request.identity.execution_id.clone())
    );
    assert!(matches!(
        inspect(&executor, &secret, &request).await,
        ExecutionReply::Inspected {
            state: ExecutionState::Revoked,
            ..
        }
    ));
    executor
        .retire_execution(
            request.identity.execution_id.clone(),
            request.control_tokens.lifecycle.clone(),
        )
        .await
        .unwrap();
    assert_eq!(
        changes.try_recv().unwrap(),
        (3, request.identity.execution_id.clone())
    );
    assert!(matches!(
        inspect(&executor, &secret, &request).await,
        ExecutionReply::Retired(_)
    ));
    assert!(changes.try_recv().is_err());
    // Failed persistence and read-only requests do not invent a Changed notification.
    let (request, held) = private(&executor, temp.path());
    executor
        .store
        .fail_execution_write
        .store(1, std::sync::atomic::Ordering::SeqCst);
    assert!(
        executor
            .prepare_objects(request.clone(), descriptors(&held))
            .await
            .is_err()
    );
    assert!(changes.try_recv().is_err());
    executor
        .dispatch(&secret, ExecutionRequest::Revoke(consume(&request)))
        .await;
    assert_eq!(
        changes.try_recv().unwrap(),
        (4, request.identity.execution_id.clone())
    );
}

fn helper(temp: &tempfile::TempDir) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let shim = temp.path().join("helper");
    let body = temp.path().join("helper-body");
    let executable = std::env::current_exe().unwrap();
    let quoted = executable.to_str().unwrap().replace('\'', "'\"'\"'");
    std::fs::write(&shim, format!("#!/bin/sh {}\n", body.display())).unwrap();
    std::fs::write(
        &body,
        format!(
            "E1A_CONFIG_FD=$3 E1A_REPORT_FD=$4 exec '{}' --ignored --exact {} --nocapture\n",
            quoted, "execution::protocol_tests::private_helper_entry"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700)).unwrap();
    shim
}

#[test]
#[ignore = "isolated production scope helper entry"]
fn private_helper_entry() {
    scope_helper::run(
        std::env::var("E1A_CONFIG_FD").unwrap().parse().unwrap(),
        std::env::var("E1A_REPORT_FD").unwrap().parse().unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn private_registration_released_at_drained_not_retire() {
    let temp = tempfile::tempdir().unwrap();
    let secret = CapabilityToken::fresh().unwrap();
    let mut executor = Executor::open(ExecutorConfig {
        journal_directory: temp.path().join("journal"),
        helper_program: helper(&temp),
        kernel_client_secret: secret.clone(),
    })
    .unwrap();
    // The shim only routes bootstrap FDs; the actual helper image is this isolated test binary.
    let image = std::fs::metadata(std::env::current_exe().unwrap()).unwrap();
    Arc::get_mut(&mut executor).unwrap().mechanism = ScopeMechanismIdentity {
        executable_device: image.dev(),
        executable_inode: image.ino(),
        protocol_version: 1,
    };
    let (request, held) = private(&executor, temp.path());
    let (_, mut changes) = executor.watch().unwrap();
    prepare(&executor, &request, &held).await;
    drop(held);
    assert_eq!(executor.live_objects.lock().unwrap().len(), 1);
    let launched = executor
        .dispatch(&secret, ExecutionRequest::Launch(consume(&request)))
        .await;
    assert!(
        matches!(
            launched,
            ExecutionReply::Receipt {
                state: ExecutionState::Spawned,
                ..
            }
        ),
        "{launched:?} output={:?}",
        executor
            .outputs
            .lock()
            .unwrap()
            .get(&request.identity.execution_id)
            .unwrap()
            .snapshot(None)
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if matches!(
            inspect(&executor, &secret, &request).await,
            ExecutionReply::Inspected {
                state: ExecutionState::Drained { .. },
                output_end: OutputEnd::Complete { .. },
                ..
            }
        ) {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_released(&executor);
    let record = executor.record(&request.identity.execution_id).unwrap();
    assert!(record.durably_settled());
    assert!(
        executor
            .outputs
            .lock()
            .unwrap()
            .contains_key(&request.identity.execution_id)
    );
    assert!(
        !executor
            .tombstones
            .lock()
            .unwrap()
            .contains_key(&request.identity.execution_id)
    );
    // Prepared, Reserved, Forking, Spawned, Exited, Drained, OutputEnd, plus any
    // output appends from the helper shim. Chunk boundaries are not a protocol fact.
    let mut sequence = 0;
    while let Ok(change) = changes.try_recv() {
        sequence += 1;
        assert_eq!(change, (sequence, request.identity.execution_id.clone()));
    }
    assert!(sequence >= 7);
    let tombstone = executor
        .retire_execution(
            request.identity.execution_id.clone(),
            request.control_tokens.lifecycle.clone(),
        )
        .await
        .unwrap();
    assert!(matches!(
        tombstone,
        ExecutionReply::Retired(ExecutionTombstone {
            slot: Some((_, 1)),
            outcome: TombstoneOutcome::Retired,
            ..
        })
    ));
    assert_eq!(
        changes.try_recv().unwrap(),
        (sequence + 1, request.identity.execution_id.clone())
    );
    assert!(
        !executor
            .store
            .output_path(&request.identity.execution_id)
            .exists()
    );
    drop(executor);
    let reopened = self::executor(&temp, &secret);
    assert!(matches!(
        inspect(&reopened, &secret, &request).await,
        ExecutionReply::Retired(_)
    ));
    assert!(
        reopened
            .legacy_allowed(&request.identity.process_alias)
            .is_err(),
        "retirement lost the durable typed alias fence"
    );
}

#[tokio::test]
async fn private_prepare_inspect_refuses_pending_settlement() {
    let temp = tempfile::tempdir().unwrap();
    let secret = CapabilityToken::fresh().unwrap();
    let executor = executor(&temp, &secret);
    let path = temp.path().join("source");
    std::fs::create_dir(&path).unwrap();
    let (request, held) = private(&executor, &path);
    prepare(&executor, &request, &held).await;
    drop(held);
    std::fs::rename(&path, temp.path().join("old-source")).unwrap();
    std::fs::create_dir(&path).unwrap();
    executor
        .store
        .fail_execution_write
        .store(1, std::sync::atomic::Ordering::SeqCst);
    assert!(matches!(
        executor
            .dispatch(&secret, ExecutionRequest::Launch(consume(&request)))
            .await,
        ExecutionReply::Refused { .. }
    ));
    let pending = executor.record(&request.identity.execution_id).unwrap();
    assert!(matches!(pending.phase, Phase::NoChild { .. }));
    assert!(!pending.durably_settled());
    assert_eq!(executor.live_objects.lock().unwrap().len(), 1);
    assert!(
        matches!(
            inspect(&executor, &secret, &request).await,
            ExecutionReply::Refused { .. }
        ),
        "Inspect must not report an undurable terminal fact"
    );
    assert!(
        !executor
            .record(&request.identity.execution_id)
            .unwrap()
            .durably_settled(),
        "Inspect must remain read-only"
    );
    assert!(matches!(
        executor
            .dispatch(&secret, ExecutionRequest::Revoke(consume(&request)))
            .await,
        ExecutionReply::RevocationAcknowledged {
            state: ExecutionState::NoChild { .. },
            ..
        }
    ));
    assert!(matches!(
        inspect(&executor, &secret, &request).await,
        ExecutionReply::Inspected {
            state: ExecutionState::NoChild { .. },
            ..
        }
    ));
    assert_released(&executor);
}
