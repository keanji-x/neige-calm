//! Cursor-checked Engine output as an AsyncRead. Protocol decoding stays in provider.
use super::*;
use std::{
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, ReadBuf};

type ReadFuture = Pin<Box<dyn Future<Output = io::Result<(Vec<u8>, u64, bool)>> + Send>>;
pub struct EngineStdio {
    engine: ExecutionEngine,
    id: ExecutionId,
    cursor: u64,
    bytes: std::io::Cursor<Vec<u8>>,
    end: bool,
    pending: Option<ReadFuture>,
}
impl EngineStdio {
    pub fn new(engine: ExecutionEngine, id: ExecutionId) -> Self {
        Self {
            engine,
            id,
            cursor: 0,
            bytes: std::io::Cursor::new(vec![]),
            end: false,
            pending: None,
        }
    }
}
impl AsyncRead for EngineStdio {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            let position = this.bytes.position() as usize;
            if position < this.bytes.get_ref().len() {
                let count = output
                    .remaining()
                    .min(this.bytes.get_ref().len() - position);
                output.put_slice(&this.bytes.get_ref()[position..position + count]);
                this.bytes.set_position((position + count) as u64);
                return Poll::Ready(Ok(()));
            }
            if this.end || output.remaining() == 0 {
                return Poll::Ready(Ok(()));
            }
            if this.pending.is_none() {
                let engine = this.engine.clone();
                let id = this.id.clone();
                let from = this.cursor;
                this.pending = Some(Box::pin(async move {
                    loop {
                        let generation = engine.output_generation().await.map_err(io_error)?;
                        match engine.output(&id, Some(from)).await {
                            Ok(ExecutionReply::Observation {
                                cursor,
                                output,
                                output_end,
                                ..
                            }) => {
                                if cursor.checked_sub(output.len() as u64) != Some(from) {
                                    return Err(io::Error::new(
                                        io::ErrorKind::InvalidData,
                                        "provider output gap",
                                    ));
                                }
                                if matches!(output_end, OutputEnd::Incomplete { .. }) {
                                    return Err(io::Error::new(
                                        io::ErrorKind::InvalidData,
                                        "provider output incomplete",
                                    ));
                                }
                                let end = matches!(output_end, OutputEnd::Complete { .. });
                                if !output.is_empty() || end {
                                    return Ok((output, cursor, end));
                                }
                            }
                            // Before a spawned receipt there is no readable capability yet.
                            Err(DriveError::Executor(ExecError::Refused(_))) => {}
                            Err(error) => return Err(io_error(error)),
                            Ok(_) => {
                                return Err(io::Error::other("unexpected provider output reply"));
                            }
                        }
                        generation.changed().await.map_err(io_error)?;
                    }
                }));
            }
            match this.pending.as_mut().unwrap().as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(result) => {
                    this.pending = None;
                    let (bytes, cursor, end) = result?;
                    this.bytes = std::io::Cursor::new(bytes);
                    this.cursor = cursor;
                    this.end = end;
                }
            }
        }
    }
}
fn io_error(error: impl std::fmt::Debug) -> io::Error {
    io::Error::other(format!("{error:?}"))
}
