use super::*;
use crate::terminal_renderer::{ClientPumpContext, SharedOwnerRegistry, run_client_pump};
use calm_session::terminal_session::{OwnerRegistry, RenderPlane};
use calm_session::{ClientCapabilities, ClientMsg, InitialScrollback, PROTOCOL_VERSION, PtySize};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use uuid::Uuid;

struct Client {
    id: Uuid,
    input: mpsc::Sender<ClientMsg>,
    output: mpsc::Receiver<DaemonMsg>,
    pump: JoinHandle<anyhow::Result<()>>,
}
impl Drop for Client {
    fn drop(&mut self) {
        self.pump.abort();
    }
}
async fn client(
    barrier: Arc<crate::terminal_renderer::InputBarrier>,
    registry: SharedOwnerRegistry,
    control: mpsc::UnboundedSender<SupervisorControl>,
    events: broadcast::Sender<DaemonMsg>,
) -> Client {
    scoped_client(
        barrier,
        registry,
        control,
        events,
        crate::terminal_renderer::ClientInputScope::InteractiveUser,
    )
    .await
}
async fn scoped_client(
    barrier: Arc<crate::terminal_renderer::InputBarrier>,
    registry: SharedOwnerRegistry,
    control: mpsc::UnboundedSender<SupervisorControl>,
    events: broadcast::Sender<DaemonMsg>,
    scope: crate::terminal_renderer::ClientInputScope,
) -> Client {
    let (input, incoming) = mpsc::channel(8);
    let (outgoing, output) = mpsc::channel(32);
    let id = Uuid::new_v4();
    let pump = tokio::spawn(run_client_pump(
        incoming,
        outgoing,
        ClientPumpContext {
            input_barrier: barrier,
            input_scope: scope,
            event_rx: events.subscribe(),
            event_tx: events,
            render_plane: Arc::new(Mutex::new(RenderPlane::new(80, 24, 1024, 20))),
            exit: Arc::new(Mutex::new(None)),
            supervisor_tx: control,
            owner_registry: registry,
            session_id: Uuid::new_v4(),
            terminal_id: "fence".into(),
        },
    ));
    input
        .send(ClientMsg::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            terminal_id: "fence".into(),
            client_id: id,
            desired_size: PtySize {
                cols: 80,
                rows: 24,
                pixel_width: None,
                pixel_height: None,
            },
            cell_size: None,
            initial_scrollback: InitialScrollback::None,
            resume_from: None,
            role_hint: None,
            capabilities: ClientCapabilities {
                render_encodings: vec![calm_session::RenderEncoding::Vt],
                supports_scrollback: true,
                supports_sixel: false,
                supports_images: false,
                kernel_originated_input: false,
            },
        })
        .await
        .unwrap();
    let mut client = Client {
        id,
        input,
        output,
        pump,
    };
    assert!(matches!(
        client.output.recv().await,
        Some(DaemonMsg::ServerHello { .. })
    ));
    client
}
async fn owner_changed(client: &mut Client) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if matches!(client.output.recv().await, Some(DaemonMsg::OwnerChanged { owner_client_id: Some(id) }) if id == client.id) { return; }
        }
    }).await.unwrap();
}

#[tokio::test]
async fn queued_connection_input_is_revoked_before_the_supervisor_write() {
    let barrier = Arc::new(crate::terminal_renderer::InputBarrier::default());
    let registry = Arc::new(Mutex::new(OwnerRegistry::new()));
    let (events, _) = broadcast::channel(32);
    let (control, mut queue) = mpsc::unbounded_channel();
    let mut old = client(
        barrier.clone(),
        registry.clone(),
        control.clone(),
        events.clone(),
    )
    .await;
    old.input
        .send(ClientMsg::Input {
            data: b"stale".to_vec(),
            input_seq: 1,
        })
        .await
        .unwrap();
    // Hold the real pump's queued work until another connection has taken over.
    let queued = tokio::time::timeout(Duration::from_secs(2), queue.recv())
        .await
        .unwrap()
        .unwrap();
    let mut next = client(barrier.clone(), registry.clone(), control.clone(), events).await;
    next.input.send(ClientMsg::OwnerClaim).await.unwrap();
    owner_changed(&mut next).await;
    control.send(queued).unwrap();
    let (writer, mut peer) = UnixStream::pair().unwrap();
    let task = spawn_supervisor_control_writer(writer, "term:fence".into(), queue);
    let outcome = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            tokio::select! {
                sent = read_frame::<ControlMsg,_>(&mut peer) => return Err(format!("stale input reached supervisor: {sent:?}")),
                message = old.output.recv() => if matches!(message,Some(DaemonMsg::ProtocolError { code:calm_session::ProtocolErrorCode::NotOwner,.. })) { return Ok(()); },
            }
        }
    }).await;
    task.abort();
    let _ = task.await;
    assert!(matches!(outcome, Ok(Ok(()))), "{outcome:?}");
}

#[tokio::test]
async fn takeover_waits_for_the_in_flight_physical_write_acknowledgement() {
    let barrier = Arc::new(crate::terminal_renderer::InputBarrier::default());
    let registry = Arc::new(Mutex::new(OwnerRegistry::new()));
    let (events, _) = broadcast::channel(32);
    let (control, queue) = mpsc::unbounded_channel();
    let old = client(
        barrier.clone(),
        registry.clone(),
        control.clone(),
        events.clone(),
    )
    .await;
    let mut next = client(barrier.clone(), registry.clone(), control.clone(), events).await;
    let (writer, mut peer) = UnixStream::pair().unwrap();
    let task = spawn_supervisor_control_writer(writer, "term:fence".into(), queue);
    old.input
        .send(ClientMsg::Input {
            data: b"accepted".to_vec(),
            input_seq: 1,
        })
        .await
        .unwrap();
    assert!(matches!(
        read_frame::<ControlMsg, _>(&mut peer).await.unwrap(),
        ControlMsg::WriteStdin(_)
    ));
    next.input.send(ClientMsg::OwnerClaim).await.unwrap();
    let premature = tokio::time::timeout(Duration::from_millis(100), owner_changed(&mut next))
        .await
        .is_ok();
    let held_owner = registry.lock().unwrap().current_owner();
    write_frame(&mut peer, &ControlReply::WriteAck { write_seq: 1 })
        .await
        .unwrap();
    if !premature {
        owner_changed(&mut next).await;
    }
    task.abort();
    let _ = task.await;
    assert!(
        !premature,
        "takeover was acknowledged before the prior physical write completed"
    );
    assert_eq!(held_owner, Some(old.id));
    assert_eq!(registry.lock().unwrap().current_owner(), Some(next.id));
}

#[tokio::test]
async fn lost_acknowledgement_blocks_new_control_and_queued_writes() {
    let barrier = Arc::new(crate::terminal_renderer::InputBarrier::default());
    let registry = Arc::new(Mutex::new(OwnerRegistry::new()));
    let (events, _) = broadcast::channel(32);
    let (control, queue) = mpsc::unbounded_channel();
    let old = client(
        barrier.clone(),
        registry.clone(),
        control.clone(),
        events.clone(),
    )
    .await;
    let mut next = client(barrier.clone(), registry.clone(), control.clone(), events).await;
    let (writer, mut peer) = UnixStream::pair().unwrap();
    let task = spawn_supervisor_control_writer(writer, "term:fence".into(), queue);
    old.input
        .send(ClientMsg::Input {
            data: b"uncertain".to_vec(),
            input_seq: 1,
        })
        .await
        .unwrap();
    assert!(matches!(
        read_frame::<ControlMsg, _>(&mut peer).await.unwrap(),
        ControlMsg::WriteStdin(_)
    ));
    // A dropped writer future has not cancelled the supervisor's physical write.
    task.abort();
    let _ = task.await;
    next.input.send(ClientMsg::OwnerClaim).await.unwrap();
    let refusal = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(DaemonMsg::ProtocolError {
                code: calm_session::ProtocolErrorCode::NotOwner,
                ..
            }) = next.output.recv().await
            {
                return;
            }
        }
    })
    .await;
    assert!(refusal.is_ok());
    assert_eq!(registry.lock().unwrap().current_owner(), Some(old.id));
    assert!(barrier.grant().await.is_none());
}

mod task_scope;
