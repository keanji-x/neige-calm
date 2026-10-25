//! Real supervisor + SQLite boundary tests. Values deliberately differ from the kernel host.
use crate::{
    db::sqlite::SqlxRepo,
    execution_store::{SqliteExecutionStore, tests::NoopProjection},
    operation::forge_action_adapter::tests::executor,
};
use std::{ffi::OsString, sync::Arc};
use workspace_runtime::execution::{self as execution, session_executor::SessionExecutor, *};

const SOCK: &str = "/wx-supervisor-synthetic-agent.sock";
const CA: &str = "/wx-supervisor-synthetic-ca.pem";
const KERNEL_SOCK: &str = "/wx-kernel-synthetic-agent.sock";
const KERNEL_CA: &str = "/wx-kernel-synthetic-ca.pem";
fn kernel_path() -> String {
    crate::kernel_bin_path::kernel_led_path()
        .unwrap()
        .path_utf8()
        .unwrap()
        .to_owned()
}
fn allowed() -> AmbientEnvironment {
    AmbientEnvironment::SupervisorAllowed {
        proxy_policy: AmbientProxyPolicy::RejectSecret,
    }
}
fn environment(proxy: &str) -> Vec<(OsString, OsString)> {
    proxy::PROXY_KEYS
        .iter()
        .map(|k| ((*k).into(), proxy.into()))
        .chain([
            ("SSH_AUTH_SOCK".into(), SOCK.into()),
            ("SSL_CERT_FILE".into(), CA.into()),
            ("PATH".into(), "/wx/supervisor/bin".into()),
            ("WX_UNAPPROVED".into(), "must-not-reach-child".into()),
        ])
        .collect()
}
struct Fixture {
    repo: SqlxRepo,
    supervisor: executor::Executor,
    adapter: SessionExecutor,
    store: SqliteExecutionStore,
    root: tempfile::TempDir,
}
impl Fixture {
    async fn new(proxy: &str) -> Self {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let supervisor =
            executor::Executor::with_environment(repo.db().clone(), &environment(proxy)).await;
        let secret = serde_json::from_slice(
            &std::fs::read(supervisor._root.path().join("key.json")).unwrap(),
        )
        .unwrap();
        let adapter = SessionExecutor::new(supervisor._socket.path().join("control.sock"), secret);
        let store = SqliteExecutionStore::new(repo.db().clone()).with_projection(
            execution::test_support::TEST_CONSUMER.into(),
            Arc::new(NoopProjection),
        );
        Self {
            repo,
            supervisor,
            adapter,
            store,
            root: tempfile::tempdir().unwrap(),
        }
    }
    fn intent(&self, policy: AmbientEnvironment) -> ExecutionIntent {
        let mut intent = execution::test_support::intent(self.root.path().canonicalize().unwrap());
        intent.metadata.program.ambient_environment = policy;
        intent.metadata.program.program = "/bin/sh".into();
        intent.metadata.program.args = vec!["-c".into(),
            "printf '%s\\n' \"${SSH_AUTH_SOCK-unset}\" \"${SSL_CERT_FILE-unset}\" \"${WX_UNAPPROVED-unset}\" \"$PATH\"; read -r done".into()];
        intent.metadata.program.environment = vec![("PATH".into(), kernel_path())];
        intent
    }
    async fn clean_persistence(&self, intent: &ExecutionIntent, retired: bool) {
        let row: (String, Option<String>) = sqlx::query_as(
            "SELECT intent_json,secrets_json FROM workspace_executions WHERE execution_id=?1",
        )
        .bind(intent.metadata.id.as_str())
        .fetch_one(self.repo.pool())
        .await
        .unwrap();
        let journal = self.supervisor._root.path().join("journal");
        let id = intent.metadata.id.as_str();
        let record = journal.join(format!(
            "{id}.{}",
            if retired { "tombstone" } else { "execution" }
        ));
        let bytes = std::fs::read_to_string(record).unwrap();
        for value in [SOCK, CA, KERNEL_SOCK, KERNEL_CA, "must-not-reach-child"] {
            assert!(!row.0.contains(value), "ambient value persisted in intent");
            assert!(
                !row.1.as_deref().unwrap_or("").contains(value),
                "ambient value persisted in secrets"
            );
            assert!(
                !bytes.contains(value),
                "ambient value persisted in supervisor record"
            );
        }
        if retired {
            assert!(!journal.join(format!("{id}.execution")).exists());
            assert!(row.1.is_none());
        }
    }
    async fn run(&self, intent: ExecutionIntent) -> String {
        let engine = Engine::new(self.store.clone(), self.adapter.clone());
        engine
            .preflight_environment(&intent.metadata.program)
            .await
            .unwrap();
        let mut tx = self.repo.pool().begin().await.unwrap();
        let _ticket = engine.admit(&mut tx, intent.clone()).await.unwrap();
        tx.commit().await.unwrap();
        assert!(matches!(
            self.adapter.prepare(&intent).await.unwrap(),
            Fact::Journal {
                state: ExecutionState::Prepared,
                ..
            }
        ));
        self.clean_persistence(&intent, false).await;
        let fact = self.adapter.launch(&intent.consume()).await.unwrap();
        let Fact::Journal {
            receipt: Some(receipt),
            state: ExecutionState::Spawned,
            ..
        } = fact
        else {
            panic!("missing spawn receipt")
        };
        let output = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let generation = self.adapter.generation().await.unwrap();
                let reply = self
                    .adapter
                    .control(ExecutionControl {
                        capability: ExecutionCapability {
                            identity: receipt.identity.clone(),
                            slot_version: receipt.slot_version,
                            purpose: ExecutionControlPurpose::Observe,
                            token: intent.secrets.tokens.observe.clone(),
                        },
                        action: ProcessAction::Observe { from_cursor: None },
                    })
                    .await
                    .unwrap();
                if let ExecutionReply::Observation { output, .. } = reply
                    && output.iter().filter(|b| **b == b'\n').count() == 4
                {
                    break String::from_utf8(output).unwrap();
                }
                generation.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        self.clean_persistence(&intent, false).await;
        self.adapter
            .control(ExecutionControl {
                capability: ExecutionCapability {
                    identity: receipt.identity.clone(),
                    slot_version: receipt.slot_version,
                    purpose: ExecutionControlPurpose::Interactive,
                    token: intent.secrets.tokens.interactive.clone(),
                },
                action: ProcessAction::WriteStdin {
                    bytes: b"done\n".to_vec(),
                },
            })
            .await
            .unwrap();
        let fact = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let generation = self.adapter.generation().await.unwrap();
                let fact = self.adapter.inspect(&intent.metadata.id).await.unwrap();
                if matches!(&fact, Fact::Journal { state, output: OutputEnd::Complete { .. }, .. } if state.permits_handoff()) { break fact; }
                generation.changed().await.unwrap();
            }
        }).await.unwrap();
        let Fact::Journal { state, .. } = fact else {
            unreachable!()
        };
        self.store.spawned(&intent.metadata.id).await.unwrap();
        self.store
            .settle(&intent.metadata.id, &Settlement::Journal(state))
            .await
            .unwrap();
        self.clean_persistence(&intent, false).await;
        assert!(matches!(
            self.adapter
                .retire(&intent.metadata.id, &intent.secrets.tokens.lifecycle)
                .await
                .unwrap(),
            Fact::Retired(_)
        ));
        self.store.finish(&intent.metadata.id).await.unwrap();
        self.clean_persistence(&intent, true).await;
        output
    }
}

#[tokio::test]
async fn w_owned_ambient_spawn_uses_supervisor_environment_without_persistence() {
    // A dedicated kernel test process avoids mutating the shared test runner's environment.
    if std::env::var("WX_AMBIENT_KERNEL_CHILD").is_err() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "operation::owned_execution::ambient_tests::w_owned_ambient_spawn_uses_supervisor_environment_without_persistence", "--nocapture"])
            .env("WX_AMBIENT_KERNEL_CHILD", "1").env("SSH_AUTH_SOCK", KERNEL_SOCK)
            .env("SSL_CERT_FILE", KERNEL_CA).status().unwrap();
        assert!(status.success());
        return;
    }
    assert_eq!(std::env::var("SSH_AUTH_SOCK").unwrap(), KERNEL_SOCK);
    assert_eq!(std::env::var("SSL_CERT_FILE").unwrap(), KERNEL_CA);
    let f = Fixture::new("http://proxy.example:8080").await;
    let output = f.run(f.intent(allowed())).await;
    assert_eq!(output, format!("{SOCK}\n{CA}\nunset\n{}\n", kernel_path()));
}
#[tokio::test]
async fn w_owned_explicit_environment_overrides_ambient_and_keeps_kernel_path() {
    let f = Fixture::new("http://proxy.example:8080").await;
    let mut intent = f.intent(allowed());
    intent
        .metadata
        .program
        .environment
        .push(("SSH_AUTH_SOCK".into(), "/explicit.sock".into()));
    assert_eq!(
        f.run(intent).await,
        format!("/explicit.sock\n{CA}\nunset\n{}\n", kernel_path())
    );
}
#[tokio::test]
async fn w_owned_none_policy_preserves_explicit_only() {
    let f = Fixture::new("http://user:secret@proxy.example").await;
    assert_eq!(
        f.run(f.intent(AmbientEnvironment::None)).await,
        format!("unset\nunset\nunset\n{}\n", kernel_path())
    );
}
#[tokio::test]
async fn w_owned_old_specification_keeps_explicit_only() {
    let f = Fixture::new("http://user:secret@proxy.example").await;
    let mut intent = f.intent(AmbientEnvironment::None);
    let mut old = serde_json::to_value(&intent.metadata.program).unwrap();
    old.as_object_mut().unwrap().remove("version");
    old.as_object_mut().unwrap().remove("ambient_environment");
    intent.metadata.program = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(
        intent.metadata.program.ambient_environment,
        AmbientEnvironment::None
    );
    old["version"] = serde_json::json!(EXECUTION_SPECIFICATION_VERSION);
    assert!(serde_json::from_value::<ExecutionSpecification>(old).is_err());
    assert_eq!(
        f.run(intent).await,
        format!("unset\nunset\nunset\n{}\n", kernel_path())
    );
}
#[tokio::test]
async fn w_owned_secret_proxy_refused_before_prepare_and_admit() {
    use std::os::unix::ffi::OsStringExt;
    for value in [
        OsString::from("http://user:secret@proxy.example"),
        OsString::from("malformed"),
        OsString::from_vec(b"host\xff".to_vec()),
    ] {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let mut env = environment("http://proxy.example:8080");
        env.push(("https_proxy".into(), value));
        let supervisor = executor::Executor::with_environment(repo.db().clone(), &env).await;
        let secret = serde_json::from_slice(
            &std::fs::read(supervisor._root.path().join("key.json")).unwrap(),
        )
        .unwrap();
        let adapter = SessionExecutor::new(supervisor._socket.path().join("control.sock"), secret);
        let store = SqliteExecutionStore::new(repo.db().clone()).with_projection(
            execution::test_support::TEST_CONSUMER.into(),
            Arc::new(NoopProjection),
        );
        let root = tempfile::tempdir().unwrap();
        let mut intent = execution::test_support::intent(root.path().canonicalize().unwrap());
        intent.metadata.program.ambient_environment = allowed();
        assert!(matches!(
            adapter
                .preflight_environment(&intent.metadata.program)
                .await,
            Err(ExecError::Refused(_))
        ));
        assert!(matches!(
            adapter.prepare(&intent).await,
            Err(ExecError::Refused(_))
        ));
        let engine = Engine::new(store.clone(), adapter.clone());
        let mut tx = repo.pool().begin().await.unwrap();
        assert!(engine.admit(&mut tx, intent.clone()).await.is_err());
        tx.rollback().await.unwrap();
        assert!(store.load(&intent.metadata.id).await.unwrap().is_none());
        assert!(
            !supervisor
                ._root
                .path()
                .join("journal")
                .join(format!("{}.execution", intent.metadata.id.as_str()))
                .exists()
        );
        intent
            .metadata
            .program
            .environment
            .push(("https_proxy".into(), "http://public.proxy:8080".into()));
        adapter
            .preflight_environment(&intent.metadata.program)
            .await
            .unwrap();
        intent.metadata.program.environment.clear();
        // Exclude discards all ambient proxies; the supervisor-only secret cannot slip through.
        intent.metadata.program.ambient_environment = AmbientEnvironment::SupervisorAllowed {
            proxy_policy: AmbientProxyPolicy::Exclude,
        };
        adapter
            .preflight_environment(&intent.metadata.program)
            .await
            .unwrap();
    }
    let f = Fixture::new("http://proxy.example:8080").await;
    for key in proxy::PROXY_KEYS {
        let mut intent = f.intent(allowed());
        intent
            .metadata
            .program
            .environment
            .push(((*key).into(), "http://user:secret@proxy.example".into()));
        assert!(matches!(
            f.adapter
                .preflight_environment(&intent.metadata.program)
                .await,
            Err(ExecError::Refused(_))
        ));
        assert!(matches!(
            f.adapter.prepare(&intent).await,
            Err(ExecError::Refused(_))
        ));
        let engine = Engine::new(f.store.clone(), f.adapter.clone());
        let mut tx = f.repo.pool().begin().await.unwrap();
        assert!(engine.admit(&mut tx, intent.clone()).await.is_err());
        tx.rollback().await.unwrap();
        assert!(f.store.load(&intent.metadata.id).await.unwrap().is_none());
    }
}

#[tokio::test]
async fn w_owned_exclude_policy_drops_ambient_proxies_at_spawn() {
    let f = Fixture::new("http://user:secret@proxy.example").await;
    let mut intent = f.intent(AmbientEnvironment::SupervisorAllowed {
        proxy_policy: AmbientProxyPolicy::Exclude,
    });
    intent.metadata.program.args[1] = intent.metadata.program.args[1]
        .replace("SSH_AUTH_SOCK", "HTTP_PROXY")
        .replace("SSL_CERT_FILE", "https_proxy")
        .replace("WX_UNAPPROVED", "NO_PROXY");
    assert_eq!(
        f.run(intent).await,
        format!("unset\nunset\nunset\n{}\n", kernel_path())
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn w_owned_none_large_explicit_environment_starts_linux_helper() {
    let f = Fixture::new("http://proxy.example:8080").await;
    let mut intent = f.intent(AmbientEnvironment::None);
    intent
        .metadata
        .program
        .environment
        .extend((0..3).map(|i| (format!("WX_LARGE_{i}"), "x".repeat(100_000))));
    assert_eq!(
        f.run(intent).await,
        format!("unset\nunset\nunset\n{}\n", kernel_path())
    );
}

#[tokio::test]
async fn w_owned_unsupported_peer_refused_before_intent() {
    use calm_session::{
        control::{ControlMsg, ControlReply},
        read_frame, write_frame,
    };
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let socket_root = calm_test_sockets::socket_dir("w-unsupported");
    let socket = socket_root.path().join("old.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let (seen, mut unsupported) = tokio::sync::mpsc::unbounded_channel();
    // Released v10 peers close on the unknown additive RPC. None never sends it.
    let peer = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let seen = seen.clone();
            tokio::spawn(async move {
                let Ok(ControlMsg::Execution(hello)) =
                    read_frame::<ControlMsg, _>(&mut stream).await
                else {
                    return;
                };
                let ExecutionRequest::Handshake(hello) = *hello else {
                    return;
                };
                assert_eq!(hello.required_contract, 10);
                write_frame(
                    &mut stream,
                    &ControlReply::Execution(Box::new(ExecutionReply::Handshake {
                        envelope_version: 10,
                        executor_boot: ExecutorBootId::fresh().unwrap(),
                        mode: hello.mode,
                    })),
                )
                .await
                .unwrap();
                let Ok((ControlMsg::Execution(request), _)) =
                    calm_session::control::execution::rights::receive::<ControlMsg>(&stream).await
                else {
                    return;
                };
                if matches!(*request, ExecutionRequest::PreflightEnvironment(_)) {
                    let _ = seen.send(());
                }
                // An old decoder closes on the unknown preflight ordinal.
            });
        }
    });
    let adapter = SessionExecutor::new(socket, CapabilityToken::fresh().unwrap());
    let store = SqliteExecutionStore::new(repo.db().clone()).with_projection(
        execution::test_support::TEST_CONSUMER.into(),
        Arc::new(NoopProjection),
    );
    let root = tempfile::tempdir().unwrap();
    let mut intent = execution::test_support::intent(root.path().canonicalize().unwrap());
    intent.metadata.program.ambient_environment = allowed();
    let engine = Engine::new(store.clone(), adapter.clone());
    let mut tx = repo.pool().begin().await.unwrap();
    assert!(matches!(
        adapter
            .preflight_environment(&intent.metadata.program)
            .await,
        Err(ExecError::Refused(_))
    ));
    assert!(engine.admit(&mut tx, intent.clone()).await.is_err());
    tokio::time::timeout(std::time::Duration::from_secs(5), unsupported.recv())
        .await
        .unwrap()
        .unwrap();
    tx.rollback().await.unwrap();
    assert!(store.load(&intent.metadata.id).await.unwrap().is_none());
    intent.metadata.program.ambient_environment = AmbientEnvironment::None;
    adapter
        .preflight_environment(&intent.metadata.program)
        .await
        .unwrap();
    peer.abort();
}
