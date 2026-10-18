//! Original first-input acceptance and a separate, coherent current observation.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum InitialInputReceiptVersion {
    #[serde(rename = "initial-input-v1")]
    V1,
}

/// Immutable mint evidence. It does not assert that the input is still queued or visible.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ConversationInitialInputReceipt {
    pub version: InitialInputReceiptVersion,
    pub entry_id: String,
    pub entry_rev: u32,
    pub queued_at_ms: i64,
    pub text_sha256: String,
    /// The original carrier; later recovery may move the same entry to another runtime.
    pub initial_runtime_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum ConversationInitialInputReceiptResult {
    Known {
        receipt: ConversationInitialInputReceipt,
    },
    /// A successful historical operation did not record an exact entry binding.
    LegacyUnavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TrackConversationCreated {
    #[serde(flatten)]
    pub conversation: crate::model::TrackConversationSummary,
    pub initial_input: ConversationInitialInputReceiptResult,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum InitialInputUnavailableReason {
    LegacyOperation,
    CreationIncomplete,
    RuntimeUnavailable,
    RuntimeChanged,
    ShuttingDown,
    LegacyProjection,
    Integrity,
}

/// Exact current queue ownership or provider-confirmed projection; no absence
/// conclusion is available from an empty queue and incomplete retained history.
/// Outbound only; the client decoder enforces reason/receipt correlation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema, TS)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum ConversationInitialInputObservation {
    Queued {
        receipt: ConversationInitialInputReceipt,
        #[serde(rename = "runtimeId")]
        runtime_id: String,
        #[serde(rename = "entryId")]
        entry_id: String,
        rev: u32,
        text: String,
        #[serde(rename = "textSha256")]
        text_sha256: String,
    },
    Projected {
        receipt: ConversationInitialInputReceipt,
        #[serde(rename = "runtimeId")]
        runtime_id: String,
        #[serde(rename = "itemId")]
        item_id: i64,
        #[serde(rename = "segmentIndex")]
        segment_index: u32,
        #[serde(rename = "entryRev")]
        entry_rev: u32,
    },
    Unavailable {
        reason: InitialInputUnavailableReason,
        /// Required nullable evidence: unknown presence does not erase proven acceptance.
        #[schema(required = true, nullable = true)]
        receipt: Option<ConversationInitialInputReceipt>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn initial_input_observation_schema_names_match_serialized_variants() {
        let receipt = ConversationInitialInputReceipt {
            version: InitialInputReceiptVersion::V1,
            entry_id: "actual-entry".into(),
            entry_rev: 1,
            queued_at_ms: 17,
            text_sha256: "digest".into(),
            initial_runtime_id: "original-runtime".into(),
        };
        let variants = [
            ConversationInitialInputObservation::Queued {
                receipt: receipt.clone(),
                runtime_id: "current-runtime".into(),
                entry_id: "actual-entry".into(),
                rev: 2,
                text: "current words".into(),
                text_sha256: "current-digest".into(),
            },
            ConversationInitialInputObservation::Projected {
                receipt: receipt.clone(),
                runtime_id: "current-runtime".into(),
                item_id: 42,
                segment_index: 1,
                entry_rev: 2,
            },
            ConversationInitialInputObservation::Unavailable {
                reason: InitialInputUnavailableReason::RuntimeUnavailable,
                receipt: Some(receipt),
            },
            ConversationInitialInputObservation::Unavailable {
                reason: InitialInputUnavailableReason::LegacyOperation,
                receipt: None,
            },
        ];
        let schema = serde_json::to_value(
            <ConversationInitialInputObservation as utoipa::PartialSchema>::schema(),
        )
        .unwrap();
        for variant in variants {
            let body = serde_json::to_value(variant).unwrap();
            let kind = body["kind"].as_str().unwrap();
            let shape = schema["oneOf"]
                .as_array()
                .unwrap()
                .iter()
                .find(|shape| shape["properties"]["kind"]["enum"][0] == kind)
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
            assert_eq!(properties, actual, "schema properties for {kind}");
            assert_eq!(required, actual, "required wire fields for {kind}");
        }
    }
}
