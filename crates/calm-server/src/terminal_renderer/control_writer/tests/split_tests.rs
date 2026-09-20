//! The writer's write shapes under a paused Tokio clock. The paused clock auto-advances to
//! the next timer whenever the runtime parks, and the writer's 5 s budget is a timer, so
//! nothing here parks: every wait is a yield-and-poll spin and the clock moves only on `advance`.
use super::*;
use crate::terminal_renderer::{
    PumpCommand, WriteAuthority, WriteShape, run_client_pump_with_commands,
};
use calm_session::terminal_session::InputPermission;
use std::future::Future;
use std::pin::{Pin, pin};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use tracing_subscriber::layer::Context as TracingContext;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{Layer, registry as tracing_registry};

/// Drives `future` with scheduler turns only, never parking the runtime
/// (so the paused clock never auto-advances); bounded in wall-clock time.
async fn spin<F: Future + Unpin>(future: &mut F) -> F::Output {
    let started = std::time::Instant::now();
    loop {
        if let Poll::Ready(output) =
            std::future::poll_fn(|cx| Poll::Ready(Pin::new(&mut *future).poll(cx))).await
        {
            return output;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "still pending after 10 s of wall-clock time"
        );
        tokio::task::yield_now().await;
    }
}
/// `n` scheduler turns without parking.
async fn turns(n: u32) {
    for _ in 0..n {
        tokio::task::yield_now().await;
    }
}
fn write(
    shape: WriteShape,
    data: &[u8],
    input_seq: u64,
    authority: WriteAuthority,
) -> (PtyWrite, mpsc::UnboundedReceiver<DaemonMsg>) {
    let (ack, acks) = mpsc::unbounded_channel();
    (
        PtyWrite {
            authority,
            data: data.to_vec(),
            input_seq,
            ack: Some(ack),
            shape,
        },
        acks,
    )
}
fn connection_authority(barrier: &Arc<crate::terminal_renderer::InputBarrier>) -> WriteAuthority {
    WriteAuthority::Connection {
        permission: InputPermission::Kernel,
        registry: Arc::new(Mutex::new(OwnerRegistry::new())),
        barrier: barrier.clone(),
        scope: crate::terminal_renderer::ClientInputScope::InteractiveUser,
    }
}
async fn expect_write_stdin(peer: &mut UnixStream) -> (Vec<u8>, Option<u64>) {
    match spin(&mut pin!(read_frame::<ControlMsg, _>(peer)))
        .await
        .unwrap()
    {
        ControlMsg::WriteStdin(request) => (request.bytes, request.write_seq),
        other => panic!("expected WriteStdin, got {other:?}"),
    }
}
async fn ack(peer: &mut UnixStream, write_seq: u64) {
    spin(&mut pin!(write_frame(
        peer,
        &ControlReply::WriteAck { write_seq }
    )))
    .await
    .unwrap();
}
async fn next_ack(acks: &mut mpsc::UnboundedReceiver<DaemonMsg>) -> Option<DaemonMsg> {
    spin(&mut pin!(acks.recv())).await
}
/// Polls `future` once per scheduler turn for `real_time` of wall-clock time and asserts it
/// stays pending; the future is kept, not dropped, so a frame that arrives later is read whole.
async fn stays_pending<F: Future + Unpin>(future: &mut F, real_time: Duration) {
    let started = std::time::Instant::now();
    let clock = tokio::time::Instant::now();
    let mut turns = 0u32;
    while started.elapsed() < real_time || turns < 256 {
        tokio::task::yield_now().await;
        let ready =
            std::future::poll_fn(|cx| Poll::Ready(Pin::new(&mut *future).poll(cx).is_ready()))
                .await;
        assert!(
            !ready,
            "completed after {turns} turns, {:?}",
            started.elapsed()
        );
        turns += 1;
    }
    assert_eq!(tokio::time::Instant::now(), clock, "the paused clock moved");
}

#[tokio::test(start_paused = true)]
async fn split_trailing_cr_sends_the_cr_alone_after_the_gap_and_acks_once() {
    let (control, queue) = mpsc::unbounded_channel();
    let (writer, mut peer) = UnixStream::pair().unwrap();
    let task = spawn_supervisor_control_writer(writer, "term:fence".into(), queue);
    let (item, mut acks) = write(
        WriteShape::SplitTrailingCr,
        b"hello\r",
        1,
        WriteAuthority::TrustedKernel,
    );
    control.send(SupervisorControl::Write(item)).unwrap();
    assert_eq!(
        expect_write_stdin(&mut peer).await,
        (b"hello".to_vec(), Some(1)),
        "frame 1 is the text without its CR"
    );
    ack(&mut peer, 1).await;
    // Frame 2 and the InputAck stay pending while the paused clock stands.
    let frame_2 = {
        let mut frame_2 = pin!(read_frame::<ControlMsg, _>(&mut peer));
        stays_pending(&mut frame_2, Duration::from_millis(100)).await;
        assert!(
            matches!(acks.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "no InputAck after the first WriteAck"
        );
        tokio::time::advance(SUBMIT_CR_GAP).await;
        spin(&mut frame_2).await.unwrap()
    };
    match frame_2 {
        ControlMsg::WriteStdin(request) => {
            assert_eq!(request.bytes, b"\r".to_vec(), "frame 2 is the CR alone");
            assert_eq!(request.write_seq, Some(2));
        }
        other => panic!("expected the CR write, got {other:?}"),
    }
    turns(64).await;
    let early = acks.try_recv();
    assert!(
        matches!(early, Err(mpsc::error::TryRecvError::Empty)),
        "no InputAck before the second WriteAck: {early:?} at {:?}",
        tokio::time::Instant::now()
    );
    ack(&mut peer, 2).await;
    assert!(matches!(
        next_ack(&mut acks).await,
        Some(DaemonMsg::InputAck { input_seq: 1 })
    ));
    // The queue is a FIFO: the next item continues the physical sequence.
    let (next, mut next_acks) = write(WriteShape::Verbatim, b"x", 2, WriteAuthority::TrustedKernel);
    control.send(SupervisorControl::Write(next)).unwrap();
    assert_eq!(
        expect_write_stdin(&mut peer).await,
        (b"x".to_vec(), Some(3))
    );
    ack(&mut peer, 3).await;
    assert!(matches!(
        next_ack(&mut next_acks).await,
        Some(DaemonMsg::InputAck { input_seq: 2 })
    ));
    task.abort();
    let _ = task.await;
}

#[tokio::test(start_paused = true)]
async fn verbatim_writes_text_and_cr_in_one_frame() {
    let (control, queue) = mpsc::unbounded_channel();
    let (writer, mut peer) = UnixStream::pair().unwrap();
    let task = spawn_supervisor_control_writer(writer, "term:fence".into(), queue);
    let (item, mut acks) = write(
        WriteShape::Verbatim,
        b"hello\r",
        1,
        WriteAuthority::TrustedKernel,
    );
    control.send(SupervisorControl::Write(item)).unwrap();
    assert_eq!(
        expect_write_stdin(&mut peer).await,
        (b"hello\r".to_vec(), Some(1))
    );
    ack(&mut peer, 1).await;
    assert!(matches!(
        next_ack(&mut acks).await,
        Some(DaemonMsg::InputAck { input_seq: 1 })
    ));
    tokio::time::advance(SUBMIT_CR_GAP * 4).await;
    let mut extra = pin!(read_frame::<ControlMsg, _>(&mut peer));
    stays_pending(&mut extra, Duration::from_millis(50)).await;
    task.abort();
    let _ = task.await;
}

struct WarnCounter {
    hits: Arc<AtomicUsize>,
}
impl<S: tracing::Subscriber> Layer<S> for WarnCounter {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: TracingContext<'_, S>) {
        if event.metadata().target() == module_path!().trim_end_matches("::tests::split_tests")
            && *event.metadata().level() == tracing::Level::WARN
        {
            self.hits.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn split_trailing_cr_writes_a_lone_cr_or_a_cr_less_payload_verbatim() {
    let warnings = Arc::new(AtomicUsize::new(0));
    let _guard = tracing::subscriber::set_default(tracing_registry().with(WarnCounter {
        hits: warnings.clone(),
    }));
    let (control, queue) = mpsc::unbounded_channel();
    let (writer, mut peer) = UnixStream::pair().unwrap();
    let task = spawn_supervisor_control_writer(writer, "term:fence".into(), queue);
    for (index, (data, warned)) in [(&b"\r"[..], 1), (&b"abc"[..], 2)].into_iter().enumerate() {
        let seq = index as u64 + 1;
        let (item, mut acks) = write(
            WriteShape::SplitTrailingCr,
            data,
            seq,
            WriteAuthority::TrustedKernel,
        );
        control.send(SupervisorControl::Write(item)).unwrap();
        assert_eq!(
            expect_write_stdin(&mut peer).await,
            (data.to_vec(), Some(seq)),
            "one frame with every byte"
        );
        ack(&mut peer, seq).await;
        assert!(matches!(
            next_ack(&mut acks).await,
            Some(DaemonMsg::InputAck { input_seq }) if input_seq == seq
        ));
        assert_eq!(warnings.load(Ordering::Relaxed), warned, "{data:?}");
    }
    tokio::time::advance(SUBMIT_CR_GAP * 4).await;
    let mut extra = pin!(read_frame::<ControlMsg, _>(&mut peer));
    stays_pending(&mut extra, Duration::from_millis(50)).await;
    task.abort();
    let _ = task.await;
}

#[tokio::test(start_paused = true)]
async fn supervisor_lost_between_the_two_writes_leaves_the_input_unknown() {
    let barrier = Arc::new(crate::terminal_renderer::InputBarrier::default());
    let (control, queue) = mpsc::unbounded_channel();
    let (writer, mut peer) = UnixStream::pair().unwrap();
    let task = spawn_supervisor_control_writer(writer, "term:fence".into(), queue);
    let (item, mut acks) = write(
        WriteShape::SplitTrailingCr,
        b"hello\r",
        1,
        connection_authority(&barrier),
    );
    control.send(SupervisorControl::Write(item)).unwrap();
    assert_eq!(
        expect_write_stdin(&mut peer).await,
        (b"hello".to_vec(), Some(1))
    );
    ack(&mut peer, 1).await;
    // Let the writer take the ack and enter its gap before the peer goes.
    turns(256).await;
    drop(peer);
    tokio::time::advance(SUBMIT_CR_GAP).await;
    assert!(next_ack(&mut acks).await.is_none(), "no ack and no refusal");
    let _ = task.await;
    assert!(barrier.grant().await.is_none());
}

async fn queued(
    queue: &mut mpsc::UnboundedReceiver<SupervisorControl>,
) -> (Vec<u8>, u64, WriteShape) {
    match tokio::time::timeout(Duration::from_secs(2), queue.recv())
        .await
        .unwrap()
        .unwrap()
    {
        SupervisorControl::Write(write) => (write.data, write.input_seq, write.shape),
        _ => panic!("expected a queued write"),
    }
}

#[tokio::test]
async fn pump_maps_the_command_shape_and_keeps_wire_input_verbatim() {
    let barrier = Arc::new(crate::terminal_renderer::InputBarrier::default());
    let registry = Arc::new(Mutex::new(OwnerRegistry::new()));
    let (events, _) = broadcast::channel(32);
    let (control, mut queue) = mpsc::unbounded_channel();
    let (input, incoming) = mpsc::channel(8);
    let (commands, commands_rx) = mpsc::channel(1);
    let (outgoing, output) = mpsc::channel(32);
    let id = Uuid::new_v4();
    let pump = tokio::spawn(run_client_pump_with_commands(
        incoming,
        Some(commands_rx),
        outgoing,
        ClientPumpContext {
            input_barrier: barrier,
            input_scope: crate::terminal_renderer::ClientInputScope::InteractiveUser,
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
    client.input.send(ClientMsg::OwnerClaim).await.unwrap();
    owner_changed(&mut client).await;
    commands
        .send(PumpCommand::Input {
            data: b"hello\r".to_vec(),
            input_seq: 1,
            shape: WriteShape::SplitTrailingCr,
        })
        .await
        .unwrap();
    assert_eq!(
        queued(&mut queue).await,
        (b"hello\r".to_vec(), 1, WriteShape::SplitTrailingCr)
    );
    client
        .input
        .send(ClientMsg::Input {
            data: b"hello\r".to_vec(),
            input_seq: 2,
        })
        .await
        .unwrap();
    assert_eq!(
        queued(&mut queue).await,
        (b"hello\r".to_vec(), 2, WriteShape::Verbatim)
    );
    commands
        .send(PumpCommand::Input {
            data: b"plain".to_vec(),
            input_seq: 3,
            shape: WriteShape::Verbatim,
        })
        .await
        .unwrap();
    assert_eq!(
        queued(&mut queue).await,
        (b"plain".to_vec(), 3, WriteShape::Verbatim)
    );
}
