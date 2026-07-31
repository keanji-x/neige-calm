//! Issue #955 §5 (PR-a) — proposal-channel wire vocabulary.
//!
//! The proposal channel was withdrawn in #973. These types remain only
//! to deserialize historical `Event::ProposalSubmitted` and
//! `Event::ProposalResolved` events.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

/// How a pending proposal was resolved. Two events, four decisions
/// (design §5.6): `accepted` / `rejected` / `stale` are user-driven
/// adjudications (`stale` is the accept attempt whose in-tx anchoring
/// checks failed); `withdrawn` is the submitting plugin reclaiming its
/// own pending slot — the only plugin-side exit, so quota can't be
/// pinned forever by abandoned proposals.
///
/// Wire shape: bare lowercase string (matches the surrounding
/// event-payload enum conventions, e.g. `EditAuthor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub enum ProposalDecision {
    /// User accepted; the report change (Batch apply) commits in the
    /// SAME write transaction as the decision event (design §5.6).
    Accepted,
    /// User rejected; no report change.
    Rejected,
    /// User pressed accept but an in-tx anchoring check failed
    /// (`if_rev` mismatch, unknown block, moved base heads). The
    /// authoritative stale verdict only exists inside the accept
    /// transaction — until then the proposal stays pending.
    Stale,
    /// Submitting plugin reclaimed its own pending proposal.
    Withdrawn,
}

impl ProposalDecision {
    /// Stable lowercase discriminator — the string persisted into the
    /// `proposals.status` projection column and rendered into
    /// role-gate denial messages. Must match the serde encoding above.
    pub fn as_str(&self) -> &'static str {
        match self {
            ProposalDecision::Accepted => "accepted",
            ProposalDecision::Rejected => "rejected",
            ProposalDecision::Stale => "stale",
            ProposalDecision::Withdrawn => "withdrawn",
        }
    }
}

/// Position anchor for proposed block creation / moves (design §5.2.1).
///
/// Proposals are asynchronous, so positions are expressed against
/// stable block ids — never numeric indexes (an unrelated insertion
/// would silently shift index semantics while every block rev still
/// matches). Externally-tagged serde gives `"at_start"` / `"at_end"` /
/// `{"after_block_id": "b_0001"}` on the wire; `after_block_id` may
/// reference a block created earlier in the same proposal via the
/// `temp:<temp_id>` form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub enum ProposalAnchor {
    /// Place directly after the referenced block (`b_xxxx`, or
    /// `temp:<temp_id>` for a block minted earlier in the batch).
    AfterBlockId(String),
    /// Place at the head of the document.
    AtStart,
    /// Place at the tail of the document.
    AtEnd,
}

/// One proposed mutation of the wave-report block document
/// (design §5.2.1). A deliberately *stricter* sibling of the
/// interactive `calm.report.blocks.*` tool DTOs: anchoring must be
/// complete because apply happens asynchronously, and the wholesale
/// `WriteMarkdown` / `Replace` shapes are excluded on purpose (full
/// overwrites and string matching cannot be meaningfully proposed).
///
/// Field-requirement rules (`if_rev` mandatory when replacing, exactly
/// one of `block_id` / `temp_id`, anchor mandatory for creations) are
/// historical: before the channel was withdrawn in #973, these constraints
/// were enforced by its submit handler. The wire type keeps them `Option`
/// only where two legal shapes share a variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub enum ProposalOp {
    /// Replace an existing block (`block_id` + mandatory `if_rev`) or
    /// create a new one (`temp_id` + mandatory `anchor`; the durable
    /// `b_xxxx` id is minted by the kernel at apply time). `payload`
    /// is the kind-specific block payload — `{ "markdown": … }` for
    /// prose, the schema-validated object for data kinds (the same
    /// vocabulary `calm.report.blocks.kinds` documents).
    UpsertBlock {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        block_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        temp_id: Option<String>,
        kind: String,
        #[ts(type = "unknown")]
        #[schema(value_type = Object)]
        payload: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        if_rev: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        anchor: Option<ProposalAnchor>,
    },
    /// Reorder an existing block. `if_rev` is mandatory (async
    /// anchoring must be complete, unlike the interactive move tool).
    MoveBlock {
        block_id: String,
        if_rev: u32,
        anchor: ProposalAnchor,
    },
    /// Delete an existing block; `if_rev` mandatory.
    DeleteBlock { block_id: String, if_rev: u32 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decision_wire_shape_is_bare_lowercase() {
        for (decision, wire) in [
            (ProposalDecision::Accepted, "\"accepted\""),
            (ProposalDecision::Rejected, "\"rejected\""),
            (ProposalDecision::Stale, "\"stale\""),
            (ProposalDecision::Withdrawn, "\"withdrawn\""),
        ] {
            assert_eq!(serde_json::to_string(&decision).unwrap(), wire);
            let back: ProposalDecision = serde_json::from_str(wire).unwrap();
            assert_eq!(back, decision);
            assert_eq!(format!("\"{}\"", decision.as_str()), wire);
        }
    }

    #[test]
    fn anchor_wire_shape_pinned() {
        assert_eq!(
            serde_json::to_value(ProposalAnchor::AtStart).unwrap(),
            json!("at_start")
        );
        assert_eq!(
            serde_json::to_value(ProposalAnchor::AtEnd).unwrap(),
            json!("at_end")
        );
        assert_eq!(
            serde_json::to_value(ProposalAnchor::AfterBlockId("b_0001".into())).unwrap(),
            json!({ "after_block_id": "b_0001" })
        );
    }

    #[test]
    fn op_wire_shape_round_trips() {
        let ops = vec![
            ProposalOp::UpsertBlock {
                block_id: None,
                temp_id: Some("t1".into()),
                kind: "prose".into(),
                payload: json!({ "markdown": "# New\n" }),
                if_rev: None,
                anchor: Some(ProposalAnchor::AtEnd),
            },
            ProposalOp::UpsertBlock {
                block_id: Some("b_0001".into()),
                temp_id: None,
                kind: "prose".into(),
                payload: json!({ "markdown": "edited\n" }),
                if_rev: Some(3),
                anchor: None,
            },
            ProposalOp::MoveBlock {
                block_id: "b_0002".into(),
                if_rev: 1,
                anchor: ProposalAnchor::AfterBlockId("temp:t1".into()),
            },
            ProposalOp::DeleteBlock {
                block_id: "b_0003".into(),
                if_rev: 2,
            },
        ];
        let wire = serde_json::to_value(&ops).unwrap();
        // Absent optionals must be absent on the wire, not null.
        assert_eq!(
            wire[0],
            json!({
                "op": "upsert_block",
                "temp_id": "t1",
                "kind": "prose",
                "payload": { "markdown": "# New\n" },
                "anchor": "at_end",
            })
        );
        assert_eq!(
            wire[3],
            json!({ "op": "delete_block", "block_id": "b_0003", "if_rev": 2 })
        );
        let back: Vec<ProposalOp> = serde_json::from_value(wire).unwrap();
        assert_eq!(back, ops);
    }
}
