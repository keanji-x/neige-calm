//! Receipt-specific facts written only after the ACP owner joins confirmed cleanup.
use super::AcpCanonicalInput;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpCleanupVersion {
    V1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpCleanupObservation {
    StoppedRevokedNormallyJoined,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcpKeptCleanup {
    pub version: AcpCleanupVersion,
    pub worker_session_id: String,
    pub client_id: String,
    pub turn_id: String,
    pub thread_id: String,
    pub native_session_id: String,
    pub association: AcpCanonicalInput,
    pub registration_digest: String,
    pub keep_key: String,
    pub keep_principal: String,
    pub keep_fingerprint: String,
    pub observation: AcpCleanupObservation,
}
