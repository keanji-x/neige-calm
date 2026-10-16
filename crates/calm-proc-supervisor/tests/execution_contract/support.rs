use calm_session::control::{IoMode, execution::*};
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
use workspace::identity::{WorkspaceCwd, WorkspaceId};

pub(super) struct Harness {
    pub(super) temp: tempfile::TempDir,
    pub(super) socket: PathBuf,
    pub(super) secret: CapabilityToken,
    pub(super) child: Child,
}

impl Harness {
    pub(super) async fn start() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("control.sock");
        let secret = CapabilityToken::fresh().unwrap();
        std::fs::write(
            temp.path().join("kernel-key.json"),
            serde_json::to_vec(&secret).unwrap(),
        )
        .unwrap();
        let child = Self::spawn(temp.path(), &socket);
        let harness = Self {
            temp,
            socket,
            secret,
            child,
        };
        let _ = harness.connect().await;
        harness
    }
    pub(super) fn spawn(root: &Path, socket: &Path) -> Child {
        Command::new(env!("CARGO_BIN_EXE_calm-proc-supervisor"))
            .arg("--control-sock")
            .arg(socket)
            .arg("--execution-state-dir")
            .arg(root.join("executor"))
            .arg("--kernel-client-key-file")
            .arg(root.join("kernel-key.json"))
            .env("NEIGE_EXECUTION_AMBIENT_SENTINEL", "must-not-reach-target")
            .env("TOKIO_WORKER_THREADS", "2")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap()
    }
    pub(super) async fn connect(&self) -> RegisteredExecutorConnection {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(connection) =
                RegisteredExecutorConnection::connect(&self.socket, self.secret.clone()).await
            {
                return connection;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "executor did not start"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    pub(super) fn root(&self, name: &str, git: bool) -> DirectoryRegistration {
        let root = self.temp.path().join(name);
        std::fs::create_dir(&root).unwrap();
        if git {
            assert!(
                Command::new("git")
                    .args(["init", "-q"])
                    .arg(&root)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let metadata = std::fs::metadata(&root).unwrap();
        DirectoryRegistration {
            path: root,
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
    pub(super) fn journal(&self) -> PathBuf {
        self.temp.path().join("executor")
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // This concrete child handle remains unreaped, so its PID cannot be reused.
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGTERM);
        }
        let _ = self.child.wait();
    }
}

pub(super) fn authority() -> ExecutionAuthority {
    ExecutionAuthority {
        grant_id: ExecutionGrantId::fresh().unwrap(),
        authorization_binding: AuthorizationBindingId::fresh().unwrap(),
        authorization_version: 3,
    }
}

pub(super) fn worktree(
    root: DirectoryRegistration,
    authority: &ExecutionAuthority,
) -> (ExecutionScope, ExecutionRegistration) {
    let workspace_id =
        WorkspaceId::try_from(ExecutionId::fresh().unwrap().as_str().to_owned()).unwrap();
    let binding_id = WorkspaceBindingId::fresh().unwrap();
    let scope = ExecutionScope::Worktree {
        workspace_id: workspace_id.clone(),
        binding_id: binding_id.clone(),
        binding_version: 4,
        generation: 7,
        install_epoch: 11,
        cwd: WorkspaceCwd::root(),
    };
    let registration = ExecutionRegistration::Worktree {
        workspace_id,
        root,
        binding_id,
        binding_version: 4,
        generation: 7,
        install_epoch: 11,
        admission_open: true,
        authority: authority.clone(),
    };
    (scope, registration)
}

pub(super) fn external(
    root: DirectoryRegistration,
    authority: &ExecutionAuthority,
) -> (ExecutionScope, ExecutionRegistration) {
    let directory_id = ExternalDirectoryId::fresh().unwrap();
    (
        ExecutionScope::ExternalDir {
            directory_id: directory_id.clone(),
            registration_version: 9,
            cwd: WorkspaceCwd::root(),
        },
        ExecutionRegistration::ExternalDir {
            directory_id,
            root,
            registration_version: 9,
            admission_open: true,
            authority: authority.clone(),
        },
    )
}

pub(super) fn prepare(
    client: &RegisteredExecutorConnection,
    scope: ExecutionScope,
    authority: ExecutionAuthority,
    alias: &str,
    io_mode: IoMode,
    script: &str,
) -> PrepareExecution {
    let receipt = client
        .registrations()
        .iter()
        .rev()
        .find(|receipt| match (&scope, &receipt.transfer.registration) {
            (
                ExecutionScope::Worktree {
                    workspace_id,
                    binding_id,
                    ..
                },
                ExecutionRegistration::Worktree {
                    workspace_id: registered,
                    binding_id: binding,
                    ..
                },
            ) => workspace_id == registered && binding_id == binding,
            (
                ExecutionScope::ExternalDir { directory_id, .. },
                ExecutionRegistration::ExternalDir {
                    directory_id: registered,
                    ..
                },
            ) => directory_id == registered,
            _ => false,
        })
        .expect("test launch requires a real attachment receipt");
    PrepareExecution {
        identity: ExecutionIdentity {
            execution_id: ExecutionId::fresh().unwrap(),
            executor_boot: client.executor_boot().clone(),
            ticket_id: LaunchTicketId::fresh().unwrap(),
            source_registration: receipt.id.clone(),
            source_incarnation: receipt.transfer.incarnation.clone(),
            authority,
            scope,
            operation_id: "explicit-test-operation".into(),
            specification_id: SpecificationId::fresh().unwrap(),
            process_alias: alias.into(),
        },
        ticket: CapabilityToken::fresh().unwrap(),
        control_tokens: ExecutionControlTokens::fresh().unwrap(),
        handoff: None,
        specification: ExecutionSpecification {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            environment: vec![
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("TERM".into(), "xterm".into()),
            ],
            io_mode,
            replay_bytes: 65536,
        },
    }
}

pub(super) fn consume(request: &PrepareExecution) -> ConsumeExecution {
    ConsumeExecution {
        execution_id: request.identity.execution_id.clone(),
        ticket_id: request.identity.ticket_id.clone(),
        ticket: request.ticket.clone(),
    }
}

pub(super) fn capability(
    request: &PrepareExecution,
    receipt: &ExecutionReceipt,
    purpose: ExecutionControlPurpose,
) -> ExecutionCapability {
    let token = match purpose {
        ExecutionControlPurpose::Observe => request.control_tokens.observe.clone(),
        ExecutionControlPurpose::Interactive => request.control_tokens.interactive.clone(),
        ExecutionControlPurpose::Lifecycle => request.control_tokens.lifecycle.clone(),
    };
    ExecutionCapability {
        identity: request.identity.clone(),
        slot_version: receipt.slot_version,
        token,
        purpose,
    }
}

pub(super) async fn register(
    client: &mut RegisteredExecutorConnection,
    registration: ExecutionRegistration,
) {
    use workspace::execution::objects::HeldSourceObjects;
    let (root, worktree) = match &registration {
        ExecutionRegistration::Worktree { root, .. } => (root, true),
        ExecutionRegistration::ExternalDir { root, .. } => (root, false),
    };
    let held = HeldSourceObjects::open(&root.path, worktree).unwrap();
    let incarnation = client
        .registrations()
        .iter()
        .find(
            |receipt| match (&receipt.transfer.registration, &registration) {
                (
                    ExecutionRegistration::Worktree {
                        workspace_id: a, ..
                    },
                    ExecutionRegistration::Worktree {
                        workspace_id: b, ..
                    },
                ) => a == b,
                (
                    ExecutionRegistration::ExternalDir {
                        directory_id: a, ..
                    },
                    ExecutionRegistration::ExternalDir {
                        directory_id: b, ..
                    },
                ) => a == b,
                _ => false,
            },
        )
        .map(|receipt| receipt.transfer.incarnation.clone())
        .unwrap_or_else(|| {
            if worktree {
                SourceObjectIncarnation::Worktree(SourceIncarnationId::fresh().unwrap())
            } else {
                SourceObjectIncarnation::ExternalDir(
                    ExternalDirectoryIncarnationId::fresh().unwrap(),
                )
            }
        });
    let transfer = AttachRegistration {
        executor_boot: client.executor_boot().clone(),
        nonce: TransferNonce::fresh().unwrap(),
        incarnation,
        registration,
        shape: held.shape().clone(),
    };
    client.attach(transfer, &held).await.unwrap();
}

pub(super) async fn launch(
    client: &mut RegisteredExecutorConnection,
    request: &PrepareExecution,
) -> ExecutionReceipt {
    assert!(matches!(
        client
            .request(ExecutionRequest::Prepare(request.clone()))
            .await
            .unwrap(),
        ExecutionReply::Prepared
    ));
    match client
        .request(ExecutionRequest::Launch(consume(request)))
        .await
        .unwrap()
    {
        ExecutionReply::Receipt {
            receipt: Some(receipt),
            state: ExecutionState::Spawned,
        } => receipt,
        other => panic!("launch did not return an actual fork: {other:?}"),
    }
}

pub(super) async fn observe(
    client: &mut RegisteredExecutorConnection,
    request: &PrepareExecution,
    receipt: &ExecutionReceipt,
) -> (ExecutionState, String) {
    let reply = client
        .request(ExecutionRequest::Control(ExecutionControl {
            capability: capability(request, receipt, ExecutionControlPurpose::Observe),
            action: ProcessAction::Observe { from_cursor: None },
        }))
        .await
        .unwrap();
    match reply {
        ExecutionReply::Observation { state, output, .. } => {
            (state, String::from_utf8_lossy(&output).into_owned())
        }
        other => panic!("observation refused: {other:?}"),
    }
}

pub(super) async fn wait_for(
    client: &mut RegisteredExecutorConnection,
    request: &PrepareExecution,
    receipt: &ExecutionReceipt,
    predicate: impl Fn(&ExecutionState, &str) -> bool,
) -> (ExecutionState, String) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (state, output) = observe(client, request, receipt).await;
        if predicate(&state, &output) {
            return (state, output);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "execution did not reach expected state: {state:?} {output:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
