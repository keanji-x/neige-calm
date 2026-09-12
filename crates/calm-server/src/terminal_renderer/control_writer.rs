use calm_session::control::{
    ControlMsg, ControlReply, ResizePtyRequest, SignalRequest, WriteStdinRequest,
};
use calm_session::{DaemonMsg, read_frame, write_frame};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::{PtyWrite, SupervisorControl};

/// Message of the `NotOwner` protocol error an admitted-then-revoked input
/// receives from the writer (the input's lease or scope went away before the
/// physical write). Kernel clients match on it to tell an input refusal apart
/// from an ownership-claim refusal.
pub const INPUT_REVOKED_BEFORE_WRITE: &str =
    "terminal input control or scope was revoked before write";

// Copied from crates/calm-session/src/bin/daemon.rs::spawn_supervisor_control_writer as part of #388 Phase 3a lift. Daemon binary retires in 3c; until then we live with duplication.
pub fn spawn_supervisor_control_writer(
    mut control_conn: UnixStream,
    proc_id: String,
    mut rx: mpsc::UnboundedReceiver<SupervisorControl>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut physical_sequence = 0u64;
        while let Some(item) = rx.recv().await {
            match item {
                SupervisorControl::Write(write) => {
                    let PtyWrite {
                        data,
                        input_seq,
                        ack,
                        authority,
                    } = write;
                    let Some(mut guard) = authority.admit().await else {
                        if let Some(ack) = ack {
                            let _ = ack.send(DaemonMsg::ProtocolError {
                                code: calm_session::ProtocolErrorCode::NotOwner,
                                message: INPUT_REVOKED_BEFORE_WRITE.into(),
                                expected_version: None,
                            });
                        }
                        continue;
                    };
                    let Some(next) = physical_sequence.checked_add(1) else {
                        break;
                    };
                    physical_sequence = next;
                    guard.started();
                    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                        write_frame(
                            &mut control_conn,
                            &ControlMsg::WriteStdin(WriteStdinRequest {
                                proc_id: proc_id.clone(),
                                bytes: data,
                                write_seq: Some(next),
                            }),
                        )
                        .await?;
                        match read_frame::<ControlReply, _>(&mut control_conn).await? {
                            ControlReply::WriteAck { write_seq } if write_seq == next => {
                                Ok::<_, anyhow::Error>(())
                            }
                            _ => anyhow::bail!("supervisor did not acknowledge terminal write"),
                        }
                    })
                    .await;
                    match result {
                        Ok(Ok(())) => {
                            guard.completed();
                            if input_seq > 0
                                && let Some(ack) = ack
                            {
                                let _ = ack.send(DaemonMsg::InputAck { input_seq });
                            }
                        }
                        error => {
                            tracing::warn!(
                                ?error,
                                "terminal write outcome unknown; closing writer"
                            );
                            break;
                        }
                    }
                }
                SupervisorControl::Resize { cols, rows } => {
                    if cols == 0 || rows == 0 {
                        continue;
                    }
                    if let Err(e) = write_frame(
                        &mut control_conn,
                        &ControlMsg::ResizePty(ResizePtyRequest {
                            proc_id: proc_id.clone(),
                            cols,
                            rows,
                            pixel_w: 0,
                            pixel_h: 0,
                        }),
                    )
                    .await
                    {
                        tracing::warn!(error = %e, "failed to send supervisor ResizePty");
                        break;
                    }
                    match read_frame::<ControlReply, _>(&mut control_conn).await {
                        Ok(ControlReply::ResizeOk) => {}
                        Ok(other) => {
                            tracing::warn!(reply = ?other, "unexpected supervisor resize reply")
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to read supervisor ResizeOk");
                            break;
                        }
                    }
                }
                SupervisorControl::Signal(sig) => {
                    if let Err(e) = write_frame(
                        &mut control_conn,
                        &ControlMsg::Signal(SignalRequest {
                            proc_id: proc_id.clone(),
                            sig,
                        }),
                    )
                    .await
                    {
                        tracing::warn!(error = %e, "failed to send supervisor Signal");
                        break;
                    }
                    match read_frame::<ControlReply, _>(&mut control_conn).await {
                        Ok(ControlReply::SignalOk) => {}
                        Ok(other) => {
                            tracing::warn!(reply = ?other, "unexpected supervisor signal reply")
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to read supervisor SignalOk");
                            break;
                        }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests;
