//! Read-only observations are bound to this exchange; serialized wire reports are not capabilities.
use super::*;
use workspace::execution::objects::{DirectoryRelationOutcome, ObjectRole};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisteredDirectoryRoot {
    pub receipt: ObjectRegistrationReceipt,
    pub role: ObjectRole,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryRelationRequest {
    pub executor_boot: ExecutorBootId,
    pub nonce: TransferNonce,
    pub ancestor: RegisteredDirectoryRoot,
    pub subject: RegisteredDirectoryRoot,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryRelationReport {
    pub request: DirectoryRelationRequest,
    pub outcome: DirectoryRelationOutcome,
}

/// Constructed only by the negotiated client after this exchange's provenance/rights checks.
/// Still only a current observation: the caller owns lifecycle fencing and version revalidation.
pub struct ObservedDirectoryRelation(DirectoryRelationReport);
impl ObservedDirectoryRelation {
    pub fn request(&self) -> &DirectoryRelationRequest {
        &self.0.request
    }
    pub fn outcome(&self) -> &DirectoryRelationOutcome {
        &self.0.outcome
    }
}

impl RegisteredExecutorConnection {
    pub async fn observe_directories(
        &mut self,
        request: DirectoryRelationRequest,
    ) -> std::io::Result<ObservedDirectoryRelation> {
        if request.executor_boot != self.executor_boot
            || [&request.ancestor, &request.subject].iter().any(|root| {
                root.role != ObjectRole::Root
                    || root.receipt.contract != EXECUTION_ENVELOPE_VERSION
                    || root.receipt.transfer.executor_boot != self.executor_boot
            })
        {
            return Err(std::io::Error::other(
                "directory observation boot/role mismatch",
            ));
        }
        self.begin()?;
        rights::send(
            &self.stream,
            &super::super::ControlMsg::Execution(ExecutionRequest::ObserveDirectories(
                request.clone(),
            )),
            &[],
        )
        .await?;
        let (reply, descriptors) =
            rights::receive::<super::super::ControlReply>(&self.stream).await?;
        if !descriptors.is_empty() {
            return Err(std::io::Error::other(
                "directory observation returned unexpected rights",
            ));
        }
        match reply {
            super::super::ControlReply::Execution(ExecutionReply::DirectoryRelation(report))
                if report.request == request =>
            {
                self.in_flight = false;
                Ok(ObservedDirectoryRelation(report))
            }
            super::super::ControlReply::Execution(ExecutionReply::Refused { reason }) => {
                self.in_flight = false;
                Err(std::io::Error::other(reason))
            }
            _ => Err(std::io::Error::other(
                "directory observation provenance mismatch",
            )),
        }
    }
}

#[cfg(test)]
mod tests;
