use super::*;
use calm_session::control::ProcSignal;

async fn signal(peer: &mut UnixStream, expected: ProcSignal) {
    let message = tokio::time::timeout(Duration::from_secs(5), read_frame::<ControlMsg, _>(peer))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(message, ControlMsg::Signal(SignalRequest { sig, .. }) if sig == expected));
    write_frame(peer, &ControlReply::SignalOk).await.unwrap();
}

#[tokio::test]
async fn stop_finishes_after_exit_and_sender_close() {
    let (control, queue) = mpsc::unbounded_channel();
    let (exited, exit_persisted) = tokio::sync::watch::channel(false);
    let (writer, mut peer) = UnixStream::pair().unwrap();
    let task = spawn_supervisor_control_writer(writer, "term:stop".into(), queue, exit_persisted);
    control.send(SupervisorControl::Stop).unwrap();
    signal(&mut peer, ProcSignal::Hup).await;
    let hup = tokio::time::Instant::now();
    exited.send(true).unwrap();
    drop(control);
    signal(&mut peer, ProcSignal::Kill).await;
    assert!(hup.elapsed() >= Duration::from_secs(2));
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert!(read_frame::<ControlMsg, _>(&mut peer).await.is_err());
}

#[tokio::test]
async fn exit_drains_accepted_stop_without_starting_queued_write() {
    let (control, queue) = mpsc::unbounded_channel();
    let (ack, mut replies) = mpsc::unbounded_channel();
    control
        .send(SupervisorControl::Write(PtyWrite {
            authority: crate::terminal_renderer::WriteAuthority::TrustedKernel,
            data: b"never started".to_vec(),
            input_seq: 1,
            ack: Some(ack),
            shape: WriteShape::Verbatim,
        }))
        .unwrap();
    control.send(SupervisorControl::Stop).unwrap();
    let (exited, exit_persisted) = tokio::sync::watch::channel(false);
    exited.send(true).unwrap();
    let (writer, mut peer) = UnixStream::pair().unwrap();
    let task = spawn_supervisor_control_writer(writer, "term:race".into(), queue, exit_persisted);
    // Both the queued controls and exit are ready before the writer's first poll.
    signal(&mut peer, ProcSignal::Hup).await;
    assert!(
        control.is_closed(),
        "exit must close admission before draining Stop"
    );
    assert!(
        replies.recv().await.is_none(),
        "unstarted write cannot receive a successful ACK"
    );
    signal(&mut peer, ProcSignal::Kill).await;
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert!(read_frame::<ControlMsg, _>(&mut peer).await.is_err());
}

#[tokio::test]
async fn cancelled_stop_cannot_send_delayed_kill() {
    let (control, queue) = mpsc::unbounded_channel();
    let (writer, mut peer) = UnixStream::pair().unwrap();
    let task = spawn_supervisor_control_writer(
        writer,
        "term:cancel".into(),
        queue,
        tokio::sync::watch::channel(false).1,
    );
    control.send(SupervisorControl::Stop).unwrap();
    signal(&mut peer, ProcSignal::Hup).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(read_frame::<ControlMsg, _>(&mut peer).await.is_err());
    assert!(control.is_closed());
}

#[tokio::test]
async fn observer_kill_is_refused_before_stop_admission() {
    let barrier = Arc::new(crate::terminal_renderer::InputBarrier::default());
    let registry = Arc::new(Mutex::new(OwnerRegistry::new()));
    let (events, _) = broadcast::channel(32);
    let (control, mut queue) = mpsc::unbounded_channel();
    let _owner = client(
        barrier.clone(),
        registry.clone(),
        control.clone(),
        events.clone(),
    )
    .await;
    let mut observer = client(barrier, registry, control, events).await;
    observer.input.send(ClientMsg::Kill).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                observer.output.recv().await,
                Some(DaemonMsg::ProtocolError {
                    code: calm_session::ProtocolErrorCode::NotOwner,
                    ..
                })
            ) {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        queue.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}
