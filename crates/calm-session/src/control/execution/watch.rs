use super::*;
use std::io;

/// A dedicated executor-wide stream. Reconnect and inspect after any transport loss or seq gap.
pub struct ExecutionWatcher {
    stream: tokio::net::UnixStream,
    boot: ExecutorBootId,
    seq: u64,
}
impl ExecutionWatcher {
    pub fn boot(&self) -> &ExecutorBootId {
        &self.boot
    }
    pub fn sequence(&self) -> u64 {
        self.seq
    }
    pub async fn changed(&mut self) -> io::Result<(u64, ExecutionId)> {
        let (reply, descriptors) =
            rights::receive::<super::super::ControlReply>(&self.stream).await?;
        if !descriptors.is_empty() {
            return Err(io::Error::other("unexpected watch descriptors"));
        }
        if let super::super::ControlReply::Execution(reply) = reply
            && let ExecutionReply::Changed { seq, id } = *reply
            && self.seq.checked_add(1) == Some(seq)
        {
            self.seq = seq;
            return Ok((seq, id));
        }
        Err(io::Error::other(
            "executor change stream interrupted; reconnect and inspect",
        ))
    }
}
impl RegisteredExecutorConnection {
    /// Consume the connection: Watch owns it until disconnect.
    pub async fn watch(mut self) -> io::Result<ExecutionWatcher> {
        self.begin()?;
        rights::send(
            &self.stream,
            &super::super::ControlMsg::Execution(Box::new(ExecutionRequest::Watch)),
            &[],
        )
        .await?;
        let (reply, descriptors) =
            rights::receive::<super::super::ControlReply>(&self.stream).await?;
        if descriptors.is_empty()
            && let super::super::ControlReply::Execution(reply) = reply
            && let ExecutionReply::Watching { boot, seq } = *reply
            && boot == self.executor_boot
        {
            return Ok(ExecutionWatcher {
                stream: self.stream,
                boot,
                seq,
            });
        }
        Err(io::Error::other("executor watch refused"))
    }
}
