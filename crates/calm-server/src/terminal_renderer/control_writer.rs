use calm_session::control::{
    ControlMsg, ControlReply, ResizePtyRequest, SignalRequest, WriteStdinRequest,
};
use calm_session::{DaemonMsg, read_frame, write_frame};
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::{PtyWrite, SupervisorControl};

/// Gap between the two physical writes of a [`WriteShape::SplitTrailingCr`] item. Claude Code
/// classifies one stdin run of more than ~62 characters as a paste and keeps a CR inside the run
/// as part of it; a CR that arrives as its own read is an Enter.
pub const SUBMIT_CR_GAP: Duration = Duration::from_millis(40);

/// How the writer hands a [`PtyWrite`]'s bytes to the supervisor. Set by the kernel's
/// encoder for `submit` only; everything else is [`WriteShape::Verbatim`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteShape {
    /// One `WriteStdin` carrying every byte as sent.
    Verbatim,
    /// The bytes before the trailing CR as one `WriteStdin`, then after [`SUBMIT_CR_GAP`] the CR
    /// as a second one. A payload that does not end with a CR, or is the lone CR, is written verbatim.
    SplitTrailingCr,
}

/// The physical writes of one item: the bytes as sent, or text then CR.
fn write_parts(data: Vec<u8>, shape: WriteShape) -> Vec<Vec<u8>> {
    match shape {
        WriteShape::Verbatim => vec![data],
        WriteShape::SplitTrailingCr if data.len() >= 2 && data.ends_with(b"\r") => {
            let mut text = data;
            let cr = text.split_off(text.len() - 1);
            vec![text, cr]
        }
        WriteShape::SplitTrailingCr => {
            tracing::warn!(
                len = data.len(),
                "split-trailing-CR write without a trailing CR; writing verbatim"
            );
            vec![data]
        }
    }
}

/// Message of the `NotOwner` protocol error an admitted-then-revoked input receives from the
/// writer. Kernel clients match on it to tell an input refusal apart from an ownership-claim refusal.
pub const INPUT_REVOKED_BEFORE_WRITE: &str =
    "terminal input control or scope was revoked before write";

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
                        shape,
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
                    // One admitted item may be two physical writes (text, gap, CR); both sequence numbers are
                    // reserved before the first byte, and the one 5 s budget, guard and InputAck span both round trips.
                    let parts = write_parts(data, shape);
                    let first = physical_sequence;
                    let Some(last) = physical_sequence.checked_add(parts.len() as u64) else {
                        break;
                    };
                    physical_sequence = last;
                    guard.started();
                    let result = tokio::time::timeout(Duration::from_secs(5), async {
                        for (index, bytes) in parts.into_iter().enumerate() {
                            if index > 0 {
                                tokio::time::sleep(SUBMIT_CR_GAP).await;
                            }
                            let next = first + index as u64 + 1;
                            write_frame(
                                &mut control_conn,
                                &ControlMsg::WriteStdin(WriteStdinRequest {
                                    proc_id: proc_id.clone(),
                                    bytes,
                                    write_seq: Some(next),
                                }),
                            )
                            .await?;
                            match read_frame::<ControlReply, _>(&mut control_conn).await? {
                                ControlReply::WriteAck { write_seq } if write_seq == next => {}
                                _ => anyhow::bail!("supervisor did not acknowledge terminal write"),
                            }
                        }
                        Ok::<_, anyhow::Error>(())
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
