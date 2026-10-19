use calm_session::control::{IoMode, execution::*};
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
use workspace::identity::WorkspaceCwd;

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

pub(super) fn external(
    root: DirectoryRegistration,
    authority: &ExecutionAuthority,
) -> (ExecutionScope, ExecutionRegistration) {
    let directory_id = ExternalDirectoryId::fresh().unwrap();
    (
        ExecutionScope::ExternalDir {
            directory_id: directory_id.clone(),
            registration_version: 1,
            cwd: WorkspaceCwd::root(),
        },
        ExecutionRegistration::ExternalDir {
            directory_id,
            root,
            registration_version: 1,
            admission_open: true,
            authority: authority.clone(),
        },
    )
}

pub(super) fn prepare(
    client: &RegisteredExecutorConnection,
    mut scope: ExecutionScope,
    registration: &ExecutionRegistration,
    authority: ExecutionAuthority,
    alias: &str,
    io_mode: IoMode,
    script: &str,
) -> PrepareExecution {
    let ExecutionRegistration::ExternalDir { root, .. } = registration else {
        panic!("private fixture requires ExternalDir");
    };
    let held = workspace::execution::objects::HeldSourceObjects::open(&root.path, false).unwrap();
    let directory = ExternalDirectoryId::fresh().unwrap();
    let ExecutionScope::ExternalDir { directory_id, .. } = &mut scope else {
        panic!("private fixture requires ExternalDir scope");
    };
    *directory_id = directory.clone();
    let transfer = AttachRegistration {
        registration_id: ObjectRegistrationId::fresh().unwrap(),
        executor_boot: client.executor_boot().clone(),
        nonce: TransferNonce::fresh().unwrap(),
        incarnation: SourceObjectIncarnation::ExternalDir(
            ExternalDirectoryIncarnationId::fresh().unwrap(),
        ),
        registration: ExecutionRegistration::ExternalDir {
            directory_id: directory,
            root: root.clone(),
            registration_version: 1,
            admission_open: true,
            authority: authority.clone(),
        },
        shape: held.shape().clone(),
    };
    PrepareExecution {
        source: PreparedSource::Private(Box::new(transfer.clone())),
        identity: ExecutionIdentity {
            execution_id: ExecutionId::fresh().unwrap(),
            executor_boot: client.executor_boot().clone(),
            ticket_id: LaunchTicketId::fresh().unwrap(),
            source_registration: transfer.registration_id,
            source_incarnation: transfer.incarnation,
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

pub(super) async fn submit(
    client: &mut RegisteredExecutorConnection,
    request: PrepareExecution,
) -> ExecutionReply {
    let PreparedSource::Private(transfer) = &request.source else {
        panic!("private fixture required")
    };
    let held = workspace::execution::objects::HeldSourceObjects::open(&transfer.shape.root, false)
        .unwrap();
    client.prepare_private(request, &held).await.unwrap()
}

pub(super) async fn launch(
    client: &mut RegisteredExecutorConnection,
    request: &PrepareExecution,
) -> ExecutionReceipt {
    assert!(matches!(
        submit(client, request.clone()).await,
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

/// Observe the supervisor's actual native root ownership, independently of wire replies.
pub(super) fn assert_root_released(harness: &Harness, root: &DirectoryRegistration) {
    use std::os::unix::fs::MetadataExt;
    let count = std::fs::read_dir(format!("/proc/{}/fd", harness.child.id()))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter_map(|path| std::fs::metadata(path).ok())
        .filter(|metadata| metadata.dev() == root.device && metadata.ino() == root.inode)
        .count();
    assert_eq!(
        count, 0,
        "settlement left supervisor root descriptions pinned"
    );
}
