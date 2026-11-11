//! Durable owner consent to conversation use of the current deployment resources.
//! Access revocation is separate from these immutable delegation facts.
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(
    tag = "kind",
    content = "id",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum ShareScope {
    Track(String),
    Area(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum SharePurpose {
    Conversation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum ShareResourcePolicy {
    CurrentDeployment,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ShareDelegation {
    pub id: String,
    pub scope: ShareScope,
    pub approver_user_id: String,
    pub purpose: SharePurpose,
    pub resource_policy: ShareResourcePolicy,
    pub created_at: i64,
}

/// Management projection deliberately contains no token or token hash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ShareLink {
    pub delegation: ShareDelegation,
    pub revoked_at: Option<i64>,
}

/// Secret returned only by creation. No endpoint can recover it later.
#[derive(Debug, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ShareLinkCreated {
    pub link: ShareLink,
    pub token: String,
}

/// Approval is server issued. Reject request-controlled authority or policy.
#[derive(Debug, Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct CreateShareLink {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct SharedTrack {
    pub id: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct SharedTarget {
    pub scope: ShareScope,
    pub tracks: Vec<SharedTrack>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct SharedRun {
    pub card_id: String,
    #[schema(required = true, nullable = true)]
    pub phase: Option<crate::harness::HarnessPhaseTag>,
    #[schema(required = true, nullable = true)]
    pub runtime_state: Option<crate::worker::WorkerSessionState>,
}
