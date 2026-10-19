use super::*;
use std::io;
use workspace::execution::objects::HeldSourceObjects;

impl RegisteredExecutorConnection {
    /// The caller retains its original lease through the receiver's validated durable ACK.
    pub async fn attach(
        &mut self,
        transfer: AttachRegistration,
        held: &HeldSourceObjects,
    ) -> io::Result<ObjectRegistrationReceipt> {
        if transfer.executor_boot != self.executor_boot || transfer.shape != *held.shape() {
            return Err(io::Error::other("attachment boot/shape mismatch"));
        }
        held.validate()?;
        self.begin()?;
        rights::send(
            &self.stream,
            &super::super::ControlMsg::Execution(Box::new(ExecutionRequest::Attach(
                transfer.clone(),
            ))),
            &held.descriptors(),
        )
        .await?;
        let (reply, files) = rights::receive::<super::super::ControlReply>(&self.stream).await?;
        if !files.is_empty() {
            return Err(io::Error::other(
                "unexpected attachment acknowledgement descriptors",
            ));
        }
        let super::super::ControlReply::Execution(reply) = reply else {
            return Err(io::Error::other("attachment acknowledgement mismatched"));
        };
        match *reply {
            ExecutionReply::Registered(receipt)
                if receipt.contract == PERSISTED_RECEIPT_CONTRACT_VERSION
                    && receipt.id == transfer.registration_id
                    && receipt.transfer == transfer =>
            {
                self.in_flight = false;
                self.registrations.push(receipt.clone());
                Ok(receipt)
            }
            ExecutionReply::Refused { reason } => {
                self.in_flight = false;
                Err(io::Error::other(reason))
            }
            _ => Err(io::Error::other("attachment acknowledgement mismatched")),
        }
    }
}

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
            &super::super::ControlMsg::Execution(Box::new(ExecutionRequest::Prepare(request))),
            &held.descriptors(),
        )
        .await?;
        let (reply, descriptors) =
            rights::receive::<super::super::ControlReply>(&self.stream).await?;
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
