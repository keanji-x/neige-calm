//! IO-free mobile pairing and access-status wire contracts. No reusable credentials are logged.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

#[derive(Clone, Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "camelCase")]
pub struct PendingPair {
    pub id: String,
    pub device_name: String,
    pub verification_code: String,
}

#[derive(Clone, Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "camelCase")]
pub struct PairedDevice {
    pub id: String,
    pub device_name: String,
}

#[derive(Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "camelCase")]
pub struct PairingCreated {
    pub id: String,
    pub qr_payload: String,
    pub qr_image: String,
    #[ts(type = "number")]
    pub expires_in_seconds: u64,
}

#[derive(Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PairingClaim {
    pub ticket: String,
    pub device_name: String,
}

#[derive(Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "camelCase")]
pub struct PairingClaimed {
    pub id: String,
    pub secret: String,
    pub verification_code: String,
}

#[derive(Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(deny_unknown_fields)]
pub struct PairingRedeem {
    pub id: String,
    pub secret: String,
}

#[derive(Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "camelCase")]
pub struct MobileStatus {
    pub available: bool,
    #[schema(required = true, nullable = true)]
    pub public_url: Option<String>,
    pub pending: Vec<PendingPair>,
    pub devices: Vec<PairedDevice>,
}
