use super::*;

#[tokio::test]
async fn failed_hello_delivery_releases_owner_lease() {
    let registry: SharedOwnerRegistry = Arc::new(StdMutex::new(OwnerRegistry::new()));
    let (event_tx, event_rx) = broadcast::channel(4);
    let mut changes = event_tx.subscribe();
    let (supervisor_tx, _supervisor_rx) = mpsc::unbounded_channel();
    let (incoming_tx, incoming_rx) = mpsc::channel(1);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(1);
    incoming_tx.send(hello()).await.unwrap();
    drop(outgoing_rx);
    run_client_pump(
        incoming_rx,
        outgoing_tx,
        ClientPumpContext {
            input_barrier: Arc::new(calm_server::terminal_renderer::InputBarrier::default()),
            input_scope: calm_server::terminal_renderer::ClientInputScope::InteractiveUser,
            event_tx,
            event_rx,
            render_plane: Arc::new(StdMutex::new(RenderPlane::new(80, 24, 1024, 20))),
            exit: Arc::new(StdMutex::new(None)),
            supervisor_tx,
            owner_registry: registry.clone(),
            session_id: Uuid::new_v4(),
            terminal_id: TID.into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        registry.lock().unwrap().current_owner(),
        None,
        "failed ServerHello delivery leaked ownership"
    );
    assert!(matches!(
        changes.try_recv(),
        Ok(DaemonMsg::OwnerChanged {
            owner_client_id: None
        })
    ));
}

#[tokio::test]
async fn cancelled_client_pump_releases_owner_lease() {
    let registry: SharedOwnerRegistry = Arc::new(StdMutex::new(OwnerRegistry::new()));
    let (event_tx, event_rx) = broadcast::channel(4);
    let mut changes = event_tx.subscribe();
    let (supervisor_tx, _supervisor_rx) = mpsc::unbounded_channel();
    let (incoming_tx, incoming_rx) = mpsc::channel(1);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(1);
    incoming_tx.send(hello()).await.unwrap();
    let task = tokio::spawn(run_client_pump(
        incoming_rx,
        outgoing_tx,
        ClientPumpContext {
            input_barrier: Arc::new(calm_server::terminal_renderer::InputBarrier::default()),
            input_scope: calm_server::terminal_renderer::ClientInputScope::InteractiveUser,
            event_tx,
            event_rx,
            render_plane: Arc::new(StdMutex::new(RenderPlane::new(80, 24, 1024, 20))),
            exit: Arc::new(StdMutex::new(None)),
            supervisor_tx,
            owner_registry: registry.clone(),
            session_id: Uuid::new_v4(),
            terminal_id: TID.into(),
        },
    ));
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), outgoing_rx.recv())
            .await
            .unwrap(),
        Some(DaemonMsg::ServerHello {
            client_role: calm_session::Role::Owner,
            ..
        })
    ));
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(
        registry.lock().unwrap().current_owner(),
        None,
        "cancelled pump leaked ownership"
    );
    assert!(matches!(
        changes.try_recv(),
        Ok(DaemonMsg::OwnerChanged {
            owner_client_id: None
        })
    ));
    // The downstream task must also terminate when its owner future is dropped.
    while tokio::time::timeout(Duration::from_secs(2), outgoing_rx.recv())
        .await
        .unwrap()
        .is_some()
    {}
}
