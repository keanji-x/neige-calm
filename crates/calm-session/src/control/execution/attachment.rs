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
