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
