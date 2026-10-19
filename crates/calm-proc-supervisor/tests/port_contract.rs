use calm_proc_supervisor::{
    ProcRegistry, bind_control_listener, execution::ExecutorConfig, serve_with_listener,
};
use workspace_runtime::execution::{session_executor::SessionExecutor, test_support, *};

#[tokio::test]
async fn executor() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("contract.sock");
    let secret = CapabilityToken::fresh().unwrap();
    let registry = ProcRegistry::new()
        .with_executor(ExecutorConfig {
            journal_directory: temp.path().join("journal"),
            helper_program: std::fs::canonicalize(env!("CARGO_BIN_EXE_calm-proc-supervisor"))
                .unwrap(),
            kernel_client_secret: secret.clone(),
        })
        .unwrap();
    let listener = bind_control_listener(&socket).unwrap();
    let (shutdown, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(serve_with_listener(
        listener,
        socket.clone(),
        registry,
        stopped,
    ));
    let executor = SessionExecutor::new(socket, secret);
    let intent = test_support::intent(temp.path().to_owned());
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        test_support::port_contract::executor(
            &executor,
            &intent,
            &test_support::intent(temp.path().to_owned()),
        ),
    )
    .await
    .unwrap();
    drop(executor);
    shutdown.send(()).unwrap();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn executor_handshake_refusal_is_transport() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("unavailable.sock");
    let listener = bind_control_listener(&socket).unwrap();
    let (shutdown, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(serve_with_listener(
        listener,
        socket.clone(),
        ProcRegistry::new(),
        stopped,
    ));
    let executor = SessionExecutor::new(socket, CapabilityToken::fresh().unwrap());
    assert!(!executor.available());
    assert!(matches!(
        executor
            .prepare(&test_support::intent(temp.path().to_owned()))
            .await,
        Err(ExecError::Transport(_))
    ));
    drop(executor);
    shutdown.send(()).unwrap();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn c2a_engine_close_stdin_uses_inspected_receipt() {
    let h = super::execution_support::Harness::start().await;
    let executor = SessionExecutor::new(h.socket.clone(), h.secret.clone());
    let store = test_support::MemoryStore::default();
    let mut intent = test_support::intent(h.temp.path().to_owned());
    intent.metadata.program.program = "/bin/sh".into();
    intent.metadata.program.args = vec!["-c".into(), "cat; printf engine-eof".into()];
    intent.metadata.program.io_mode = calm_session::control::IoMode::Pipe;
    let id = intent.metadata.id.clone();
    let engine = Engine::new(store.clone(), executor);
    let mut tx = test_support::MemoryTx::default();
    let pending = engine.admit(&mut tx, intent).await.unwrap();
    store.commit(tx);
    engine.drive(pending.committed());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if matches!(
                engine.output(&id, None).await,
                Ok(ExecutionReply::Observation {
                    receipt: Some(_),
                    ..
                })
            ) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        engine
            .interact(
                &id,
                ProcessAction::WriteStdin {
                    bytes: b"engine-prefix".to_vec()
                }
            )
            .await
            .unwrap(),
        ExecutionReply::StdinProgress { accepted_bytes: 13 }
    ));
    engine.close_stdin(&id).await.unwrap();
    engine.close_stdin(&id).await.unwrap();
    assert!(
        engine
            .interact(&id, ProcessAction::WriteStdin { bytes: vec![] })
            .await
            .is_err()
    );
    engine.wait_settled(&id).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let ExecutionReply::Observation {
                output,
                output_end: OutputEnd::Complete { .. },
                ..
            } = engine.output(&id, None).await.unwrap()
            {
                assert_eq!(output, b"engine-prefixengine-eof");
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
