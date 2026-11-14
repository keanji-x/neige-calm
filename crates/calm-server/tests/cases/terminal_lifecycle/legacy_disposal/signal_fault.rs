//! Fault only the checked KILL reply; all process state comes from a real supervisor.
use super::*;
use calm_session::control::{ControlErrorKind, ControlMsg, ControlReply, ProcSignal};
use calm_session::{read_frame, write_frame};
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct SignalFault {
    pub sock: std::path::PathBuf,
    pub reject: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}
impl SignalFault {
    pub fn new(owner: &Owner) -> Self {
        let sock = owner._dir.path().join("fault.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let upstream = owner.sock.clone();
        let reject = Arc::new(AtomicBool::new(true));
        let fault = reject.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                let upstream = upstream.clone();
                let reject = fault.clone();
                tokio::spawn(async move {
                    let mut server = tokio::net::UnixStream::connect(upstream).await.unwrap();
                    while let Ok(msg) = read_frame::<ControlMsg, _>(&mut client).await {
                        if matches!(&msg, ControlMsg::Signal(request) if request.sig == ProcSignal::Kill)
                            && reject.load(Ordering::SeqCst)
                        {
                            let _ = write_frame(
                                &mut client,
                                &ControlReply::Error {
                                    kind: ControlErrorKind::Internal,
                                    message: "temporary KILL failure".into(),
                                },
                            )
                            .await;
                            return;
                        }
                        if write_frame(&mut server, &msg).await.is_err() {
                            return;
                        }
                        if matches!(msg, ControlMsg::Probe(_) | ControlMsg::Execution(_)) {
                            let Ok(reply) = read_frame::<ControlReply, _>(&mut server).await else {
                                return;
                            };
                            if write_frame(&mut client, &reply).await.is_err() {
                                return;
                            }
                        } else {
                            let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
                            return;
                        }
                    }
                });
            }
        });
        Self { sock, reject, task }
    }
}
impl Drop for SignalFault {
    fn drop(&mut self) {
        self.task.abort();
    }
}
