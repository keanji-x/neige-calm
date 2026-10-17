//! One-shot read-only transfer: no execution grant, persistent registration or SourceProof.
use super::*;
use std::io;
use workspace::execution::objects::{
    DirectoryPairFacts, DirectoryRelationOutcome, ObjectQualificationFailure,
    QualifiedDirectoryPair,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferredDirectoryRelationRequest {
    pub executor_boot: ExecutorBootId,
    pub nonce: TransferNonce,
    /// Exactly two directory descriptors: ancestor first, subject second.
    pub facts: DirectoryPairFacts,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferredDirectoryRelationReport {
    pub request: TransferredDirectoryRelationRequest,
    pub outcome: DirectoryRelationOutcome,
}

#[derive(Debug, thiserror::Error)]
pub enum TransferredDirectoryObservationError {
    #[error("directory observation transport: {0}")]
    Transport(#[from] io::Error),
    #[error(transparent)]
    Qualification(#[from] ObjectQualificationFailure),
    #[error("directory observation refused: {0}")]
    Refused(String),
}

/// Only the checked client exchange constructs this value. It retains the original two FD
/// borrows and qualification. Kernel must consume it under its actual owner exclusion; this
/// is directed current VFS evidence, never persistent authority or physical independence.
pub struct ObservedTransferredDirectoryRelation<'files> {
    report: TransferredDirectoryRelationReport,
    qualification: QualifiedDirectoryPair<'files>,
}
impl ObservedTransferredDirectoryRelation<'_> {
    pub fn request(&self) -> &TransferredDirectoryRelationRequest {
        &self.report.request
    }
    pub fn outcome(&self) -> &DirectoryRelationOutcome {
        &self.report.outcome
    }
    pub fn qualification(&self) -> &QualifiedDirectoryPair<'_> {
        &self.qualification
    }
    /// Object/profile continuity only; caller exclusion must protect ancestry through consumption.
    pub fn revalidate_objects(&self) -> Result<(), ObjectQualificationFailure> {
        self.qualification.revalidate()
    }
}

impl RegisteredExecutorConnection {
    pub async fn observe_transferred_directories<'files>(
        &mut self,
        qualification: QualifiedDirectoryPair<'files>,
    ) -> Result<ObservedTransferredDirectoryRelation<'files>, TransferredDirectoryObservationError>
    {
        qualification.revalidate()?;
        let request = TransferredDirectoryRelationRequest {
            executor_boot: self.executor_boot.clone(),
            nonce: TransferNonce::fresh()?,
            facts: qualification.facts().clone(),
        };
        self.begin()?;
        rights::send(
            &self.stream,
            &super::super::ControlMsg::Execution(Box::new(
                ExecutionRequest::ObserveTransferredDirectories(Box::new(request.clone())),
            )),
            &qualification.descriptors(),
        )
        .await?;
        let (reply, descriptors) =
            rights::receive::<super::super::ControlReply>(&self.stream).await?;
        if !descriptors.is_empty() {
            return Err(io::Error::other(
                "transferred directory observation returned unexpected rights",
            )
            .into());
        }
        let super::super::ControlReply::Execution(reply) = reply else {
            return Err(
                io::Error::other("transferred directory observation provenance mismatch").into(),
            );
        };
        match *reply {
            ExecutionReply::TransferredDirectoryRelation(report) if report.request == request => {
                self.in_flight = false;
                qualification.revalidate()?;
                Ok(ObservedTransferredDirectoryRelation {
                    report: *report,
                    qualification,
                })
            }
            ExecutionReply::ObjectQualificationFailed { failure } => {
                self.in_flight = false;
                Err(TransferredDirectoryObservationError::Qualification(failure))
            }
            ExecutionReply::Refused { reason } => {
                self.in_flight = false;
                Err(TransferredDirectoryObservationError::Refused(reason))
            }
            _ => Err(
                io::Error::other("transferred directory observation provenance mismatch").into(),
            ),
        }
    }
}

#[cfg(test)]
mod tests;
