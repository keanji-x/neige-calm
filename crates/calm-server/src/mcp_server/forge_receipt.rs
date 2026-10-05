//! The pending receipt for every plugin-backed forge action, regardless of tool name.
use serde::Serialize;
use serde_json::Value;

pub(super) const PENDING_GUIDANCE: &str = include_str!("../../prompts/forge-action/pending.md");

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum PendingStatus {
    Pending,
}

#[derive(Serialize)]
struct PendingReceipt<'a> {
    op_id: &'a str,
    parked: bool,
    status: PendingStatus,
    message: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    completion_event: Option<&'a str>,
}

pub(crate) fn pending(op_id: &str, completion_event: Option<&str>) -> Value {
    serde_json::to_value(PendingReceipt {
        op_id,
        parked: true,
        status: PendingStatus::Pending,
        message: PENDING_GUIDANCE.trim(),
        completion_event,
    })
    .expect("pending receipt contains only strings, a bool, and an enum")
}
