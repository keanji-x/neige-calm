use super::*;
use calm_session::{
    control::{ControlMsg, ControlReply},
    read_frame, write_frame,
};
use tokio::{net::UnixListener, sync::oneshot};

#[tokio::test]
async fn watch_reconnect_wakes_all_waiters() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("watch.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let boot = ExecutorBootId::fresh().unwrap();
    let (close, closed) = oneshot::channel();
    let (reconnected, reconnect) = oneshot::channel();
    let (finish, finished) = oneshot::channel();
    let server = tokio::spawn(async move {
        for wait in [closed, finished] {
            let (mut stream, _) = listener.accept().await.unwrap();
            assert!(
                matches!(read_frame::<ControlMsg, _>(&mut stream).await.unwrap(), ControlMsg::Execution(request) if matches!(*request, ExecutionRequest::Handshake(_)))
            );
            write_frame(
                &mut stream,
                &ControlReply::Execution(Box::new(ExecutionReply::Handshake {
                    envelope_version: EXECUTION_ENVELOPE_VERSION,
                    executor_boot: boot.clone(),
                    mode: ExecutionProtocolMode::RegisteredV3,
                })),
            )
            .await
            .unwrap();
            assert!(
                matches!(read_frame::<ControlMsg, _>(&mut stream).await.unwrap(), ControlMsg::Execution(request) if matches!(*request, ExecutionRequest::Watch))
            );
            write_frame(
                &mut stream,
                &ControlReply::Execution(Box::new(ExecutionReply::Watching {
                    boot: boot.clone(),
                    seq: 0,
                })),
            )
            .await
            .unwrap();
            wait.await.unwrap();
        }
        let _ = reconnected.send(());
    });
    let executor =
        session_executor::SessionExecutor::new(socket, CapabilityToken::fresh().unwrap());
    let a = bounded(executor.generation()).await.unwrap();
    let b = executor.generation().await.unwrap();
    close.send(()).unwrap();
    bounded(async {
        tokio::try_join!(a.changed(), b.changed()).unwrap();
    })
    .await;
    // generation blocks until Watching again, then both subscriptions see a subsequent loss.
    let a = bounded(executor.generation()).await.unwrap();
    let b = executor.generation().await.unwrap();
    assert!(executor.available());
    finish.send(()).unwrap();
    bounded(async {
        tokio::try_join!(a.changed(), b.changed()).unwrap();
    })
    .await;
    bounded(reconnect).await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn command_transport_loss_reconnects_watch() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("command-loss.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let boot = ExecutorBootId::fresh().unwrap();
    let (finish, finished) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut watch = negotiate(&listener, &boot).await;
        start_watch(&mut watch, &boot).await;
        let mut command = negotiate(&listener, &boot).await;
        assert!(
            matches!(read_frame::<ControlMsg, _>(&mut command).await.unwrap(), ControlMsg::Execution(request) if matches!(*request, ExecutionRequest::Inspect(_)))
        );
        drop(command); // Lose one command, leaving the old Watch transport alive.
        let mut reconnected = negotiate(&listener, &boot).await;
        start_watch(&mut reconnected, &boot).await;
        let _ = finished.await;
        drop((watch, reconnected));
    });
    let executor =
        session_executor::SessionExecutor::new(socket, CapabilityToken::fresh().unwrap());
    let generation = bounded(executor.generation()).await.unwrap();
    assert!(matches!(
        executor.inspect(&ExecutionId::fresh().unwrap()).await,
        Err(ExecError::Transport(_))
    ));
    bounded(generation.changed()).await.unwrap();
    bounded(executor.generation()).await.unwrap();
    assert!(executor.available());
    finish.send(()).unwrap();
    server.await.unwrap();
}

async fn negotiate(listener: &UnixListener, boot: &ExecutorBootId) -> tokio::net::UnixStream {
    let (mut stream, _) = listener.accept().await.unwrap();
    assert!(
        matches!(read_frame::<ControlMsg, _>(&mut stream).await.unwrap(), ControlMsg::Execution(request) if matches!(*request, ExecutionRequest::Handshake(_)))
    );
    write_frame(
        &mut stream,
        &ControlReply::Execution(Box::new(ExecutionReply::Handshake {
            envelope_version: EXECUTION_ENVELOPE_VERSION,
            executor_boot: boot.clone(),
            mode: ExecutionProtocolMode::RegisteredV3,
        })),
    )
    .await
    .unwrap();
    stream
}
async fn start_watch(stream: &mut tokio::net::UnixStream, boot: &ExecutorBootId) {
    assert!(
        matches!(read_frame::<ControlMsg, _>(stream).await.unwrap(), ControlMsg::Execution(request) if matches!(*request, ExecutionRequest::Watch))
    );
    write_frame(
        stream,
        &ControlReply::Execution(Box::new(ExecutionReply::Watching {
            boot: boot.clone(),
            seq: 0,
        })),
    )
    .await
    .unwrap();
}
