//! Scan-only v2 contracts. Secrets must never be formatted or persisted.
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

#[derive(Deserialize, Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnrollmentClaim {
    pub enrollment_id: String,
    pub ticket: String,
    pub device_name: String,
    pub attempt_id: String,
    pub attempt_secret: String,
}

#[derive(Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnrollmentClaimed {
    pub enrollment_id: String,
    pub attempt_id: String,
    pub claim_id: String,
}

#[derive(Deserialize, Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnrollmentRedeem {
    pub enrollment_id: String,
    pub attempt_id: String,
    pub attempt_secret: String,
}

#[derive(Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnrollmentRedeemed {
    pub enrollment_id: String,
    pub attempt_id: String,
    pub session_fingerprint: String,
}

#[derive(Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnrollmentCreated {
    pub enrollment_id: String,
    pub qr_payload: String,
    pub qr_image: String,
    #[ts(type = "number")]
    pub auth_key_expires_at: i64,
    #[ts(type = "number")]
    pub pair_expires_at: i64,
}

#[derive(Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnrollmentCleanup {
    pub pending_cleanup: u32,
    pub detail: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnrollmentAction {
    Create,
    Cancel,
    Status,
    Cleanup,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnrollmentCommand {
    pub action: EnrollmentAction,
    pub enrollment_id: String,
    pub generation: String,
    pub deadline: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentRequest {
    pub version: u32,
    pub command: EnrollmentCommand,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnrollmentResult {
    pub enrollment_id: String,
    pub generation: String,
    pub origin: String,
    pub auth_key: String,
    pub auth_key_expires_at: i64,
    pub pair_expires_at: i64,
    pub pending_cleanup: u32,
    pub detail: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentResponse {
    pub version: u32,
    pub result: Option<EnrollmentResult>,
    pub error: Option<String>,
}
