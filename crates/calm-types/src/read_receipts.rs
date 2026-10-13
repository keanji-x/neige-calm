//! Read receipts (#2596): what the one user has read, kept by the kernel so a refresh or a second
//! device reads the same state. The unread rule itself lives with its reader
//! (`fe/core/domain/read-receipts.ts`): `manualUnread || activityAt > max(baselineMs, readThroughMs)`.
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

/// What a receipt is about: a track, or one conversation. A closed set, checked by the table too.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum ReadReceiptKind {
    Track,
    Conversation,
}

impl ReadReceiptKind {
    /// The wire, path and column spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Conversation => "conversation",
        }
    }

    /// The kind a path segment or column names; `None` for anything outside the closed set.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "track" => Some(Self::Track),
            "conversation" => Some(Self::Conversation),
            _ => None,
        }
    }
}

/// One target's receipt. `readThroughMs` only rises; `manualUnread` is the reader's explicit
/// "mark unread", cleared by the next mark read.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ReadReceipt {
    pub kind: ReadReceiptKind,
    pub target_id: String,
    pub read_through_ms: i64,
    pub manual_unread: bool,
}

/// `GET /api/read-receipts`: the cutover baseline (activity at or before it is read) and every
/// stored receipt.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ReadReceipts {
    pub baseline_ms: i64,
    pub receipts: Vec<ReadReceipt>,
}

/// `POST /api/read-receipts/{kind}/{target_id}/read`: the activity instant the reader has seen.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct MarkReadRequest {
    pub read_through_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_spelling_round_trips_and_rejects_outside_the_closed_set() {
        for kind in [ReadReceiptKind::Track, ReadReceiptKind::Conversation] {
            assert_eq!(ReadReceiptKind::parse(kind.as_str()), Some(kind));
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::json!(kind.as_str())
            );
        }
        assert_eq!(ReadReceiptKind::parse("area"), None);
        assert_eq!(ReadReceiptKind::parse("Track"), None);
    }
}
