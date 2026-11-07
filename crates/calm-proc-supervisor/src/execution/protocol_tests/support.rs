use super::*;
use calm_session::control::IoMode;
use std::os::{fd::AsFd, unix::fs::MetadataExt};
use workspace::{execution::objects::HeldSourceObjects, identity::WorkspaceCwd};

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
                ambient_environment: calm_session::control::execution::AmbientEnvironment::None,
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

pub(super) fn descriptors(held: &HeldSourceObjects) -> Vec<std::os::fd::OwnedFd> {
    held.descriptors()
        .iter()
        .map(|fd| fd.as_fd().try_clone_to_owned().unwrap())
        .collect()
}

pub(super) fn consume(request: &PrepareExecution) -> ConsumeExecution {
    ConsumeExecution {
        execution_id: request.identity.execution_id.clone(),
        ticket_id: request.identity.ticket_id.clone(),
        ticket: request.ticket.clone(),
    }
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
