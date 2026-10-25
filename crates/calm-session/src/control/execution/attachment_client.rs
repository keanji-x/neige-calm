use super::*;
use std::io;
use workspace::execution::objects::HeldSourceObjects;

impl RegisteredExecutorConnection {
    /// Transfer a private root with its immutable Prepare; retain the caller's lease through ACK.
    pub async fn prepare_private(
        &mut self,
        request: PrepareExecution,
        held: &HeldSourceObjects,
    ) -> io::Result<ExecutionReply> {
        let PreparedSource::Private(_) = &request.source else {
            return Err(io::Error::other("private source required"));
        };
        // The executor dispatches exact retries before qualifying fresh FDs or the current boot.
        self.begin()?;
        rights::send(
            &self.stream,
            &super::super::ControlMsg::Execution(Box::new(
                ExecutionRequest::Prepare(request).for_wire(),
            )),
            &held.descriptors(),
        )
        .await?;
        let (reply, descriptors) =
            rights::receive_without_rights::<super::super::ControlReply>(&self.stream).await?;
        if !descriptors.is_empty() {
            return Err(io::Error::other("unexpected prepare descriptors"));
        }
        match reply {
            super::super::ControlReply::Execution(reply) => {
                self.in_flight = false;
                Ok(*reply)
            }
            _ => Err(io::Error::other("unexpected prepare reply")),
        }
    }
}
