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
            &super::super::ControlMsg::Execution(ExecutionRequest::Attach(transfer.clone())),
            &held.descriptors(),
        )
        .await?;
        let (reply, files) = rights::receive::<super::super::ControlReply>(&self.stream).await?;
        if !files.is_empty() {
            return Err(io::Error::other(
                "unexpected attachment acknowledgement descriptors",
            ));
        }
        match reply {
            super::super::ControlReply::Execution(ExecutionReply::Registered(receipt))
                if receipt.contract == EXECUTION_ENVELOPE_VERSION
                    && receipt.id == transfer.registration_id
                    && receipt.transfer == transfer =>
            {
                self.in_flight = false;
                self.registrations.push(receipt.clone());
                Ok(receipt)
            }
            super::super::ControlReply::Execution(ExecutionReply::Refused { reason }) => {
                self.in_flight = false;
                Err(io::Error::other(reason))
            }
            _ => Err(io::Error::other("attachment acknowledgement mismatched")),
        }
    }
    /// Recover the actual held objects from this same executor boot. A path never fills a gap.
    pub async fn export(
        &mut self,
        request: ExportRegistration,
    ) -> io::Result<(ObjectRegistrationReceipt, HeldSourceObjects)> {
        if request.executor_boot != self.executor_boot {
            return Err(io::Error::other("export executor boot mismatch"));
        }
        self.begin()?;
        rights::send(
            &self.stream,
            &super::super::ControlMsg::Execution(ExecutionRequest::Export(request.clone())),
            &[],
        )
        .await?;
        let (reply, files) = rights::receive::<super::super::ControlReply>(&self.stream).await?;
        let receipt = match reply {
            super::super::ControlReply::Execution(ExecutionReply::Exported { receipt, nonce })
                if receipt.contract == EXECUTION_ENVELOPE_VERSION
                    && nonce == request.nonce
                    && receipt.id == request.registration
                    && receipt.id == receipt.transfer.registration_id
                    && receipt.transfer.executor_boot == self.executor_boot =>
            {
                receipt
            }
            _ => return Err(io::Error::other("export receipt/nonce mismatch")),
        };
        let held = HeldSourceObjects::receive(receipt.transfer.shape.clone(), files)?;
        rights::send(
            &self.stream,
            &super::super::ControlMsg::Execution(ExecutionRequest::ExportAck(
                ExportAcknowledgement {
                    executor_boot: self.executor_boot.clone(),
                    registration: receipt.id.clone(),
                    nonce: request.nonce,
                },
            )),
            &[],
        )
        .await?;
        let (ack, files) = rights::receive::<super::super::ControlReply>(&self.stream).await?;
        if !files.is_empty()
            || !matches!(
                ack,
                super::super::ControlReply::Execution(ExecutionReply::ExportAcknowledged)
            )
        {
            return Err(io::Error::other("export acknowledgement refused"));
        }
        self.in_flight = false;
        self.registrations.push(receipt.clone());
        Ok((receipt, held))
    }
}
