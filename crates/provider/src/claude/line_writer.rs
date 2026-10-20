//! Framed stdin writes and factual crossing evidence.
use crate::send_boundary::PreSendGate;
use std::time::Duration;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;

pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const ADMISSION_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
pub enum LineWriteError {
    #[error("{0}")]
    BeforeWrite(String),
    #[error("{0}")]
    CrossedWrite(LineIoError),
}

/// Write one framed line. Lock wait and admission have a separate one-second budget.
/// Once the first write is polled, failure or cancellation closes stdin before unlocking:
/// a pipe write is not atomic, and a later line must never append to a partial frame.
pub async fn write_line<W: AsyncWrite + Unpin>(
    stdin: &Mutex<Option<W>>,
    line: &str,
    gate: &PreSendGate<'_>,
) -> Result<(), LineWriteError> {
    let framed = format!("{line}\n");
    let deadline = tokio::time::Instant::now() + ADMISSION_TIMEOUT;
    let mut guard = tokio::time::timeout_at(deadline, stdin.lock())
        .await
        .map_err(|_| {
            LineWriteError::BeforeWrite("claude stdin wait timed out before write".into())
        })?;
    // Take ownership before admission. Refusal restores it; cancellation closes it,
    // even if admission had not yet completed, without claiming any bytes were sent.
    let mut writer = guard
        .take()
        .ok_or_else(|| LineWriteError::BeforeWrite("claude stdin is already closed".into()))?;
    // Construct the write future before admission. Its independent write budget starts
    // only when it is first polled, immediately after the final synchronous check.
    let write = async {
        let result = tokio::time::timeout(WRITE_TIMEOUT, async {
            writer
                .write_all(framed.as_bytes())
                .await
                .map_err(|error| LineWriteError::CrossedWrite(LineIoError::Io(error)))?;
            writer
                .flush()
                .await
                .map_err(|error| LineWriteError::CrossedWrite(LineIoError::Io(error)))
        })
        .await;
        match result {
            Ok(result) => result,
            Err(_) => Err(LineWriteError::CrossedWrite(LineIoError::Timeout)),
        }
    };
    let admitted = tokio::time::timeout_at(deadline, gate.run())
        .await
        .map_err(|_| LineWriteError::BeforeWrite("claude admission timed out before write".into()))
        .and_then(|result| result.map_err(|error| LineWriteError::BeforeWrite(error.0)));
    let result = match admitted {
        Ok(()) => write.await,
        Err(error) => {
            drop(write);
            *guard = Some(writer);
            return Err(error);
        }
    };
    if result.is_ok() {
        *guard = Some(writer);
    }
    result
}

/// I/O evidence after entering the writer, including a flush of a consumed line.
#[derive(Debug, thiserror::Error)]
pub enum LineIoError {
    #[error("{0}")]
    Io(std::io::Error),
    #[error("claude did not take its stdin line in time")]
    Timeout,
}

#[cfg(test)]
mod tests;
