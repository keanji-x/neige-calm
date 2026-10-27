//! Cursor-checked Engine output as an AsyncRead. Protocol decoding stays in provider.
use super::*;
use std::{
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[derive(Default)]
pub struct WriteProgress {
    output_failed: std::sync::atomic::AtomicBool,
    stdin_failed: std::sync::atomic::AtomicBool,
    accepted: std::sync::atomic::AtomicUsize,
    uncertain: std::sync::atomic::AtomicBool,
}
impl WriteProgress {
    pub(crate) fn stop_reason(&self) -> Option<StopReason> {
        use std::sync::atomic::Ordering::SeqCst;
        if self.output_failed.load(SeqCst) {
            Some(StopReason::OutputGap)
        } else if self.stdin_failed.load(SeqCst) {
            Some(StopReason::StdinTorn)
        } else {
            None
        }
    }

    /// A lost reply cannot certify that zero bytes crossed the transport.
    pub fn crossed(&self) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        self.accepted.load(SeqCst) > 0 || self.uncertain.load(SeqCst)
    }
}
type WriteFuture = Pin<Box<dyn Future<Output = io::Result<usize>> + Send>>;
type ShutdownFuture = Pin<Box<dyn Future<Output = io::Result<()>> + Send>>;
type ReadFuture = Pin<Box<dyn Future<Output = io::Result<(Vec<u8>, u64, bool)>> + Send>>;
pub struct EngineStdio {
    engine: ExecutionEngine,
    id: ExecutionId,
    cursor: u64,
    bytes: std::io::Cursor<Vec<u8>>,
    end: bool,
    pending: Option<ReadFuture>,
    write: Option<WriteFuture>,
    shutdown: Option<ShutdownFuture>,
    torn: bool,
    closed: bool,
    progress: Arc<WriteProgress>,
}
impl EngineStdio {
    pub(crate) fn with_progress(mut self, progress: Arc<WriteProgress>) -> Self {
        self.progress = progress;
        self
    }
    pub fn write_progress(&self) -> Arc<WriteProgress> {
        self.progress.clone()
    }
    pub fn new(engine: ExecutionEngine, id: ExecutionId) -> Self {
        Self {
            engine,
            id,
            cursor: 0,
            bytes: std::io::Cursor::new(vec![]),
            end: false,
            pending: None,
            write: None,
            shutdown: None,
            torn: false,
            closed: false,
            progress: Arc::default(),
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
                                state,
                                ..
                            }) => {
                                let gap = cursor.checked_sub(output.len() as u64) != Some(from);
                                let incomplete = matches!(output_end, OutputEnd::Incomplete { .. });
                                // Record only factual lifecycle metadata, before the caller can
                                // stop the process. Output bytes and peer-supplied reasons are private.
                                if gap
                                    || incomplete
                                    || matches!(output_end, OutputEnd::Complete { .. })
                                {
                                    let (state, exit_status) = state_diagnostic(&state);
                                    if gap || incomplete {
                                        tracing::warn!(
                                            execution_id = id.as_str(),
                                            from_cursor = from,
                                            cursor,
                                            output_bytes = output.len(),
                                            state,
                                            exit_status = ?exit_status,
                                            gap,
                                            incomplete,
                                            "provider output continuity failed before cleanup"
                                        );
                                    } else {
                                        tracing::info!(
                                            execution_id = id.as_str(),
                                            from_cursor = from,
                                            cursor,
                                            output_bytes = output.len(),
                                            state,
                                            exit_status = ?exit_status,
                                            "provider output reached EOF before cleanup"
                                        );
                                    }
                                }
                                if gap {
                                    return Err(io::Error::new(
                                        io::ErrorKind::InvalidData,
                                        "provider output gap",
                                    ));
                                }
                                if incomplete {
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
                    if result.is_err() {
                        this.progress
                            .output_failed
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                    }
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

impl AsyncWrite for EngineStdio {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.torn || this.closed {
            return Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, "StdinTorn")));
        }
        if this.write.is_none() {
            let engine = this.engine.clone();
            let id = this.id.clone();
            let bytes = bytes.to_vec();
            let progress = this.progress.clone();
            this.write = Some(Box::pin(async move {
                let len = bytes.len();
                loop {
                    use std::sync::atomic::Ordering::SeqCst;
                    progress.uncertain.store(true, SeqCst);
                    let reply = engine
                        .interact(
                            &id,
                            ProcessAction::WriteStdin {
                                bytes: bytes.clone(),
                            },
                        )
                        .await;
                    match reply {
                        Ok(ExecutionReply::StdinProgress { accepted_bytes })
                            if accepted_bytes <= len =>
                        {
                            progress.accepted.fetch_add(accepted_bytes, SeqCst);
                            progress.uncertain.store(false, SeqCst);
                            if accepted_bytes > 0 || len == 0 {
                                return Ok(accepted_bytes);
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                        Err(error) => {
                            if matches!(error, DriveError::Executor(ExecError::Refused(_))) {
                                progress.uncertain.store(false, SeqCst);
                            }
                            return Err(io::Error::new(
                                io::ErrorKind::BrokenPipe,
                                format!("StdinTorn: {error:?}"),
                            ));
                        }
                        _ => {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "invalid stdin progress",
                            ));
                        }
                    }
                }
            }));
        }
        match this.write.as_mut().unwrap().as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                this.write = None;
                this.torn = result.is_err();
                if this.torn {
                    this.progress
                        .stdin_failed
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                }
                Poll::Ready(result)
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(if self.torn {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "StdinTorn"))
        } else {
            Ok(())
        })
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.closed {
            return Poll::Ready(Ok(()));
        }
        if this.shutdown.is_none() {
            let engine = this.engine.clone();
            let id = this.id.clone();
            this.shutdown = Some(Box::pin(async move {
                engine.close_stdin(&id).await.map_err(io_error)
            }));
        }
        match this.shutdown.as_mut().unwrap().as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                this.shutdown = None;
                this.closed = result.is_ok();
                this.torn |= result.is_err();
                Poll::Ready(result)
            }
        }
    }
}

/// Process state is evidence from the execution owner, never inferred from pipe EOF.
fn state_diagnostic(state: &ExecutionState) -> (&'static str, Option<i32>) {
    match state {
        ExecutionState::Prepared => ("prepared", None),
        ExecutionState::Revoked => ("revoked", None),
        ExecutionState::Forking => ("forking", None),
        ExecutionState::Spawned => ("spawned", None),
        ExecutionState::Exited { status } => ("exited", *status),
        ExecutionState::Drained { status } => ("drained", *status),
        ExecutionState::NoChild { .. } => ("no_child", None),
        ExecutionState::Unknown { .. } => ("unknown", None),
        ExecutionState::InterruptedQuiescent { .. } => ("interrupted_quiescent", None),
        ExecutionState::OriginalGroupEmpty { status } => ("original_group_empty", *status),
    }
}
