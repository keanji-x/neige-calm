use super::*;
use workspace::execution::objects::ObjectShape;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachRegistration {
    pub registration_id: ObjectRegistrationId,
    pub executor_boot: ExecutorBootId,
    pub nonce: TransferNonce,
    pub incarnation: SourceObjectIncarnation,
    pub registration: ExecutionRegistration,
    pub shape: ObjectShape,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectRegistrationReceipt {
    pub contract: u32,
    pub id: ObjectRegistrationId,
    pub transfer: AttachRegistration,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExportRegistration {
    pub executor_boot: ExecutorBootId,
    pub registration: ObjectRegistrationId,
    pub nonce: TransferNonce,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExportAcknowledgement {
    pub executor_boot: ExecutorBootId,
    pub registration: ObjectRegistrationId,
    pub nonce: TransferNonce,
}

/// The negative result only reports that this executor holds no objects for
/// this exact registration. It is neither NoChild nor source-replacement permission.
pub enum RegistrationExport {
    Held {
        receipt: ObjectRegistrationReceipt,
        objects: workspace::execution::objects::HeldSourceObjects,
    },
    Unwitnessed,
}
