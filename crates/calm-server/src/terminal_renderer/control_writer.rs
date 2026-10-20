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

enum Connection {
    Legacy(UnixStream),
    Typed {
        engine: std::sync::Arc<crate::execution_boot::ExecutionEngine>,
        id: workspace_runtime::execution::ExecutionId,
        repo: std::sync::Arc<dyn crate::db::RouteRepo>,
    },
}
impl Connection {
    async fn exchange(&mut self, message: ControlMsg) -> anyhow::Result<ControlReply> {
        use workspace_runtime::execution::{ExecutionReply, ProcessAction};
        let (engine, id, repo) = match self {
            Self::Legacy(stream) => {
                write_frame(stream, &message).await?;
                return Ok(read_frame(stream).await?);
            }
            Self::Typed { engine, id, repo } => (engine, id, repo),
        };
        match message {
            ControlMsg::WriteStdin(request) => {
                let mut bytes = request.bytes.as_slice();
                while !bytes.is_empty() {
                    let reply = engine
                        .interact(
                            id,
                            ProcessAction::WriteStdin {
                                bytes: bytes.to_vec(),
                            },
                        )
                        .await
                        .map_err(|error| {
                            let error = anyhow::anyhow!("{error:?}");
                            if bytes.len() == request.bytes.len() {
                                error
                            } else {
                                anyhow::anyhow!("partial typed write: {error}")
                            }
                        })?;
                    let ExecutionReply::StdinProgress { accepted_bytes } = reply else {
                        anyhow::bail!("unexpected typed write reply: {reply:?}");
                    };
                    anyhow::ensure!(
                        accepted_bytes <= bytes.len(),
                        "typed stdin returned an invalid prefix"
                    );
                    if accepted_bytes == 0 {
                        // A known empty prefix is safe to retry; transport loss is never retried.
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                    bytes = &bytes[accepted_bytes..];
                }
                Ok(ControlReply::WriteAck {
                    write_seq: request.write_seq.unwrap_or(0),
                })
            }
            ControlMsg::ResizePty(request) => {
                let reply = engine
                    .interact(
                        id,
                        ProcessAction::Resize {
                            cols: request.cols,
                            rows: request.rows,
                            pixel_w: request.pixel_w,
                            pixel_h: request.pixel_h,
                        },
                    )
                    .await
                    .map_err(|error| anyhow::anyhow!("{error:?}"))?;
                anyhow::ensure!(
                    matches!(reply, ExecutionReply::ActionComplete),
                    "typed resize refused"
                );
                Ok(ControlReply::ResizeOk)
            }
            ControlMsg::Signal(_) => {
                request_typed_stop(engine, id, repo).await?;
                Ok(ControlReply::SignalOk)
            }
            _ => anyhow::bail!("unsupported terminal interaction"),
        }
    }
}

pub(super) async fn request_typed_stop(
    engine: &std::sync::Arc<crate::execution_boot::ExecutionEngine>,
    id: &workspace_runtime::execution::ExecutionId,
    repo: &std::sync::Arc<dyn crate::db::RouteRepo>,
) -> anyhow::Result<()> {
    let engine_request = engine.clone();
    let request_id = id.clone();
    let ticket = crate::db::write_in_tx_typed(repo.as_ref(), move |tx| {
        Box::pin(async move {
            engine_request
                .request(tx, &request_id, workspace_runtime::execution::Desired::Stop)
                .await
                .map_err(|e| crate::error::CalmError::Internal(format!("{e:?}")))
        })
    })
    .await?;
    engine.drive(ticket.committed());
    Ok(())
}

pub fn spawn_supervisor_control_writer(
    control_conn: UnixStream,
    proc_id: String,
    rx: mpsc::UnboundedReceiver<SupervisorControl>,
) -> JoinHandle<()> {
    spawn_writer(Connection::Legacy(control_conn), proc_id, rx)
}
pub(super) fn spawn_typed_control_writer(
    engine: std::sync::Arc<crate::execution_boot::ExecutionEngine>,
    id: workspace_runtime::execution::ExecutionId,
    repo: std::sync::Arc<dyn crate::db::RouteRepo>,
    rx: mpsc::UnboundedReceiver<SupervisorControl>,
) -> JoinHandle<()> {
    spawn_writer(Connection::Typed { engine, id, repo }, String::new(), rx)
}
fn spawn_writer(
    mut connection: Connection,
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
                            let reply = connection
                                .exchange(ControlMsg::WriteStdin(WriteStdinRequest {
                                    proc_id: proc_id.clone(),
                                    bytes,
                                    write_seq: Some(next),
                                }))
                                .await
                                .map_err(|error| {
                                    if index == 0 {
                                        error
                                    } else {
                                        anyhow::anyhow!("partial terminal submission: {error}")
                                    }
                                })?;
                            match reply {
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
                    let reply = connection
                        .exchange(ControlMsg::ResizePty(ResizePtyRequest {
                            proc_id: proc_id.clone(),
                            cols,
                            rows,
                            pixel_w: 0,
                            pixel_h: 0,
                        }))
                        .await;
                    match reply {
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
                    let reply = connection
                        .exchange(ControlMsg::Signal(SignalRequest {
                            proc_id: proc_id.clone(),
                            sig,
                        }))
                        .await;
                    match reply {
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
