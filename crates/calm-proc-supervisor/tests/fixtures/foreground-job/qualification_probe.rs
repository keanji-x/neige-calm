//! Real qualified APIs and executor consumer, confined to this disposable child namespace.
use calm_session::control::{IoMode, execution::*};
use std::{
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::Path,
    time::Duration,
};
use workspace::{
    execution::objects::*,
    identity::{WorkspaceCwd, WorkspaceId},
};

pub(super) fn run(root: &Path, source: &Path, helper: &Path) -> anyhow::Result<()> {
    let original = HeldSourceObjects::open(root, true)?;
    let original_qualified = original.qualify_local()?;
    let original_identity = original.live_identity()?;
    super::bind_alias::enter_private_namespace()?;
    let namespace_changed_unknown = matches!(
        original_qualified.revalidate(),
        Err(ObjectQualificationFailure::Unknown(
            ObjectQualificationUnknown::NamespaceChanged
        ))
    );
    let current = HeldSourceObjects::open(root, true)?;
    let namespace_comparison_unsupported = original_identity.compare(&current.live_identity()?)
        == HeldObjectComparison::NamespaceUnsupported;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut result = runtime.block_on(executor_boundary(root, source, helper, &current))?;
    result["namespace_changed_unknown"] = namespace_changed_unknown.into();
    result["namespace_comparison_unsupported"] = namespace_comparison_unsupported.into();
    println!("{result}");
    Ok(())
}

async fn executor_boundary(
    root: &Path,
    source: &Path,
    helper: &Path,
    held: &HeldSourceObjects,
) -> anyhow::Result<serde_json::Value> {
    let temporary = tempfile::tempdir()?;
    let socket = temporary.path().join("qualified.sock");
    let secret = CapabilityToken::fresh()?;
    let registry = calm_proc_supervisor::ProcRegistry::new().with_executor(
        calm_proc_supervisor::execution::ExecutorConfig {
            journal_directory: temporary.path().join("journal"),
            helper_program: helper.into(),
            kernel_client_secret: secret.clone(),
        },
    )?;
    let listener = calm_proc_supervisor::bind_control_listener(&socket)?;
    let (shutdown, receiver) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(calm_proc_supervisor::serve_with_listener(
        listener,
        socket.clone(),
        registry,
        receiver,
    ));
    let mut client = RegisteredExecutorConnection::connect(&socket, secret).await?;
    let workspace = WorkspaceId::try_from(ExecutionId::fresh()?.as_str().to_owned())
        .map_err(anyhow::Error::msg)?;
    let binding = WorkspaceBindingId::fresh()?;
    let authority = ExecutionAuthority {
        grant_id: ExecutionGrantId::fresh()?,
        authorization_binding: AuthorizationBindingId::fresh()?,
        authorization_version: 1,
    };
    let root_metadata = held.root().metadata()?;
    let receipt = client
        .attach(
            AttachRegistration {
                registration_id: ObjectRegistrationId::fresh()?,
                executor_boot: client.executor_boot().clone(),
                nonce: TransferNonce::fresh()?,
                incarnation: SourceObjectIncarnation::Worktree(SourceIncarnationId::fresh()?),
                registration: ExecutionRegistration::Worktree {
                    workspace_id: workspace.clone(),
                    root: DirectoryRegistration {
                        path: root.into(),
                        device: root_metadata.dev(),
                        inode: root_metadata.ino(),
                    },
                    binding_id: binding.clone(),
                    binding_version: 1,
                    generation: 1,
                    install_epoch: 1,
                    admission_open: true,
                    authority: authority.clone(),
                },
                shape: held.shape().clone(),
            },
            held,
        )
        .await?;
    let request = PrepareExecution {
        identity: ExecutionIdentity {
            execution_id: ExecutionId::fresh()?,
            executor_boot: client.executor_boot().clone(),
            ticket_id: LaunchTicketId::fresh()?,
            source_registration: receipt.id,
            source_incarnation: receipt.transfer.incarnation,
            authority,
            scope: ExecutionScope::Worktree {
                workspace_id: workspace,
                binding_id: binding,
                binding_version: 1,
                generation: 1,
                install_epoch: 1,
                cwd: WorkspaceCwd::try_from("mounted".to_owned())
                    .map_err(|e| anyhow::anyhow!(e))?,
            },
            operation_id: "qualification-consumer".into(),
            specification_id: SpecificationId::fresh()?,
            process_alias: "qualification-probe".into(),
        },
        ticket: CapabilityToken::fresh()?,
        control_tokens: ExecutionControlTokens::fresh()?,
        handoff: None,
        specification: ExecutionSpecification {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "touch forbidden".into()],
            environment: vec![("PATH".into(), "/usr/bin:/bin".into())],
            io_mode: IoMode::Pipe,
            replay_bytes: 4096,
        },
    };
    let prepared = client
        .request(ExecutionRequest::Prepare(request.clone()))
        .await?;
    let prepare_before_bind = matches!(prepared, ExecutionReply::Prepared);
    let target = root.join("mounted");
    let source_c = std::ffi::CString::new(source.as_os_str().as_bytes())?;
    let target_c = std::ffi::CString::new(target.as_os_str().as_bytes())?;
    anyhow::ensure!(
        unsafe {
            libc::mount(
                source_c.as_ptr(),
                target_c.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            )
        } == 0,
        "private bind: {}",
        std::io::Error::last_os_error()
    );
    let cwd_cross_mount_unsupported = matches!(
        held.qualify_local()?.open_cwd(
            &WorkspaceCwd::try_from("mounted".to_owned()).map_err(|e| anyhow::anyhow!(e))?
        ),
        Err(ObjectQualificationFailure::Unsupported(
            ObjectQualificationUnsupported::CwdMountBoundary { .. }
        ))
    );
    let consume = || ConsumeExecution {
        execution_id: request.identity.execution_id.clone(),
        ticket_id: request.identity.ticket_id.clone(),
        ticket: request.ticket.clone(),
    };
    let launched = client.request(ExecutionRequest::Launch(consume())).await?;
    let launch_cross_mount_unsupported = matches!(
        launched,
        ExecutionReply::ObjectQualificationFailed {
            failure: ObjectQualificationFailure::Unsupported(
                ObjectQualificationUnsupported::CwdMountBoundary { .. }
            )
        }
    );
    // A mutation must still drain any unexpectedly created execution before the outer assertion.
    if let ExecutionReply::Receipt {
        receipt: Some(receipt),
        ..
    } = launched
    {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let observation = client
                .request(ExecutionRequest::Control(ExecutionControl {
                    capability: ExecutionCapability {
                        identity: request.identity.clone(),
                        slot_version: receipt.slot_version,
                        token: request.control_tokens.observe.clone(),
                        purpose: ExecutionControlPurpose::Observe,
                    },
                    action: ProcessAction::Observe { from_cursor: None },
                }))
                .await?;
            if matches!(
                observation,
                ExecutionReply::Observation {
                    state: ExecutionState::Drained { .. },
                    ..
                }
            ) {
                break;
            }
            anyhow::ensure!(
                tokio::time::Instant::now() < deadline,
                "unexpected execution did not drain"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    } else {
        client.request(ExecutionRequest::Revoke(consume())).await?;
    }
    let bound = HeldSourceObjects::open(&target, false)?;
    let qualified = bound.qualify_local()?;
    anyhow::ensure!(
        unsafe {
            libc::mount(
                std::ptr::null(),
                target_c.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
                std::ptr::null(),
            )
        } == 0,
        "private readonly remount: {}",
        std::io::Error::last_os_error()
    );
    let changed_mount_unknown = matches!(
        qualified.revalidate(),
        Err(ObjectQualificationFailure::Unknown(
            ObjectQualificationUnknown::MountInformationChanged { .. }
        ))
    );
    anyhow::ensure!(
        unsafe { libc::umount2(target_c.as_ptr(), libc::MNT_DETACH) } == 0,
        "detach private bind: {}",
        std::io::Error::last_os_error()
    );
    let lost_mount_unknown = matches!(
        bound.qualify_local(),
        Err(ObjectQualificationFailure::Unknown(
            ObjectQualificationUnknown::MountInformationMissing { .. }
        ))
    );
    let no_payload = !source.join("forbidden").exists() && !target.join("forbidden").exists();
    let _ = shutdown.send(());
    server.await??;
    Ok(
        serde_json::json!({ "prepare_before_bind": prepare_before_bind, "launch_cross_mount_unsupported": launch_cross_mount_unsupported, "cwd_cross_mount_unsupported": cwd_cross_mount_unsupported, "changed_mount_unknown": changed_mount_unknown, "lost_mount_unknown": lost_mount_unknown, "no_payload": no_payload }),
    )
}
