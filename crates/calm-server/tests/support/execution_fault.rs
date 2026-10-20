//! Brief control-connection outage after release, without restarting the supervisor.
use calm_session::control::{
    ControlMsg, ControlReply,
    execution::{ExecutionRequest, ProcessAction},
};
use std::path::PathBuf;

pub struct Fault {
    socket: PathBuf,
    backend: PathBuf,
    task: tokio::task::JoinHandle<()>,
    pub hit: tokio::sync::oneshot::Receiver<()>,
}
impl Drop for Fault {
    fn drop(&mut self) {
        self.task.abort();
        std::fs::remove_file(&self.socket).unwrap();
        std::fs::rename(&self.backend, &self.socket).unwrap();
    }
}
impl Fault {
    pub fn install(socket: &std::path::Path, execution: String, close: bool) -> Self {
        let backend = socket.with_extension("backend");
        std::fs::rename(socket, &backend).unwrap();
        let listener = tokio::net::UnixListener::bind(socket).unwrap();
        let (send, hit) = tokio::sync::oneshot::channel();
        let path = backend.clone();
        let task = tokio::spawn(async move {
            let mut send = Some(send);
            while let Ok((mut client, _)) = listener.accept().await {
                let mut server = tokio::net::UnixStream::connect(&path).await.unwrap();
                // Relay the documented handshake, then inspect the command frame.
                let handshake: ControlMsg = calm_session::read_frame(&mut client).await.unwrap();
                calm_session::write_frame(&mut server, &handshake)
                    .await
                    .unwrap();
                let reply: ControlReply = calm_session::read_frame(&mut server).await.unwrap();
                calm_session::write_frame(&mut client, &reply)
                    .await
                    .unwrap();
                let command: ControlMsg = match calm_session::read_frame(&mut client).await {
                    Ok(command) => command,
                    Err(_) => continue,
                };
                let selected = matches!(&command, ControlMsg::Execution(request)
                    if matches!(request.as_ref(), ExecutionRequest::Control(control)
                        if control.capability.identity.execution_id.as_str() == execution
                        && if close { matches!(control.action, ProcessAction::CloseStdin) }
                            else { matches!(control.action, ProcessAction::Observe { .. }) }));
                if selected {
                    drop(client);
                    if let Some(send) = send.take() {
                        send.send(()).unwrap();
                    }
                    continue;
                }
                calm_session::write_frame(&mut server, &command)
                    .await
                    .unwrap();
                tokio::spawn(async move {
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
                });
            }
        });
        Self {
            socket: socket.into(),
            backend,
            task,
            hit,
        }
    }
}
