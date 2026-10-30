//! Pure interpretation of original journal evidence; no lookup, retry selection or execution.
use super::planner_harness_start_adapter::HarnessProfile;
use super::{Operation, Phase, TxOutput, assistant_start_wire::StartInput};
use crate::conversation_keys::first_message_digest;
use calm_types::conversation_receipt::{
    ConversationInitialInputReceipt, ConversationInitialInputReceiptResult as ReceiptResult,
    InitialInputUnavailableReason as Reason,
};
use serde_json::Value;

/// Only the fields used by the historical receipt owner. Offline callers can borrow
/// independently inventoried fields without invoking the runtime row decoder.
pub(crate) struct HistoricalInitialInput<'a> {
    pub payload: &'a Value,
    pub target_type: &'a str,
    pub target_id: Option<&'a str>,
    pub phase: &'a Phase,
    pub tx_output: Option<&'a TxOutput>,
}

impl<'a> From<&'a Operation> for HistoricalInitialInput<'a> {
    fn from(op: &'a Operation) -> Self {
        Self {
            payload: &op.payload,
            target_type: &op.target_type,
            target_id: op.target_id.as_deref(),
            phase: &op.phase,
            tx_output: op.tx_output.as_ref(),
        }
    }
}

/// Replays evidence only; unavailable evidence never authorizes a new acceptance.
pub(crate) fn historical_initial_input_receipt(
    op: Option<HistoricalInitialInput<'_>>,
    track_id: &str,
    card_id: &str,
    key: &str,
) -> Result<ReceiptResult, Reason> {
    let Some(op) = op else {
        return Err(Reason::CreationIncomplete);
    };
    let payload = match StartInput::decode(op.payload) {
        Ok(input) => input.payload,
        Err(_) => return Err(Reason::Integrity),
    };
    if payload.profile != HarnessProfile::Assistant
        || payload.track_id != track_id
        || payload.planner_card_id.as_str() != card_id
        || payload
            .create_card
            .as_ref()
            .and_then(|seed| seed.idempotency_key.as_deref())
            != Some(key)
    {
        return Err(Reason::Integrity);
    }
    if op.target_type != "card" || op.target_id != Some(card_id) {
        return Err(Reason::Integrity);
    }
    if *op.phase != Phase::Succeeded {
        return Err(Reason::CreationIncomplete);
    }
    let Some(output) = op.tx_output else {
        return Err(Reason::Integrity);
    };
    if output.target_type != "card"
        || output.target_id.as_deref() != Some(card_id)
        || output.data.get("card_id").and_then(|v| v.as_str()) != Some(card_id)
        || output.data.get("track_id").and_then(|v| v.as_str()) != Some(track_id)
    {
        return Err(Reason::Integrity);
    }
    let Some(value) = output.data.get("initial_input_receipt") else {
        return Ok(ReceiptResult::LegacyUnavailable);
    };
    let receipt: ConversationInitialInputReceipt = match serde_json::from_value(value.clone()) {
        Ok(receipt) => receipt,
        Err(_) => return Err(Reason::Integrity),
    };
    if receipt.entry_id.is_empty()
        || receipt.queued_at_ms < 0
        || receipt.initial_runtime_id.is_empty()
        || output.data.get("runtime_id").and_then(|v| v.as_str())
            != Some(receipt.initial_runtime_id.as_str())
        || payload
            .first_message
            .as_deref()
            .map(first_message_digest)
            .as_deref()
            != Some(receipt.text_sha256.as_str())
    {
        return Err(Reason::Integrity);
    }
    Ok(ReceiptResult::Known { receipt })
}
