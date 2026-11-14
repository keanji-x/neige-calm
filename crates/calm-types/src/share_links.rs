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

/// Owner-only management projection; guest endpoints never serialize this type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ShareLink {
    pub delegation: ShareDelegation,
    pub revoked_at: Option<i64>,
    pub expires_at: i64,
}

/// Creation result; the secret is returned once and never persisted.
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

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum ShareAccessStatus {
    Locked {
        #[serde(rename = "expiresAt")]
        expires_at: i64,
    },
    Unlocked {
        scope: ShareScope,
        #[serde(rename = "expiresAt")]
        expires_at: i64,
    },
}

#[derive(Debug, Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct UnlockShareLinkRequest {
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct UnlockedShareLink {
    pub scope: ShareScope,
    pub expires_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn share_access_status_schema_matches_serialized_variants() {
        let schema =
            serde_json::to_value(<ShareAccessStatus as utoipa::PartialSchema>::schema()).unwrap();
        for variant in [
            ShareAccessStatus::Locked { expires_at: 123 },
            ShareAccessStatus::Unlocked {
                scope: ShareScope::Area("shared-area".into()),
                expires_at: 123,
            },
        ] {
            let body = serde_json::to_value(variant).unwrap();
            let status = body["status"].as_str().unwrap();
            let shape = schema["oneOf"]
                .as_array()
                .unwrap()
                .iter()
                .find(|shape| shape["properties"]["status"]["enum"][0] == status)
                .unwrap();
            let actual = body
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            let properties = shape["properties"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            let required = shape["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|name| name.as_str().unwrap())
                .collect::<BTreeSet<_>>();
            assert_eq!(properties, actual, "schema fields for {status}");
            assert_eq!(required, actual, "required wire fields for {status}");
        }
    }
}
