//! Original HarnessSnapshot root addresses, not a decoder or a normalized frame.
use super::{JsonStep, SourceLocation, SourceRef};

// calm-server::harness::snapshot::HarnessSnapshot declaration order (snapshot.rs:88).
// Defaults do not create source nodes. This closed V1 protocol is not Record's layout.
pub(super) const V1_FIELDS: [&str; 25] = [
    "schema_version",
    "mode",
    "phase",
    "turn_budget",
    "push_watermark",
    "pending_queue",
    "pending_envelope_ids",
    "pending_entry_meta",
    "pending_message_ids",
    "last_thread_id",
    "last_turn_id",
    "last_report_body_sha256",
    "last_seen_head",
    "issued_turn_head",
    "issued_input_segments",
    "projection_client_id",
    "wedged_reason",
    "interruption_intent",
    "token_usage",
    "compact_outcome",
    "planner_compaction",
    "context_usage_invalidated",
    "context_usage_invalidated_turn",
    "pending_rewind",
    "last_turn_base",
];

pub(super) fn path(path: &str, sequence: bool) -> String {
    let Some(rest) = path.strip_prefix('/') else {
        return path.into();
    };
    let (field, tail) = rest
        .split_once('/')
        .map_or((rest, ""), |(f, _)| (f, &rest[f.len()..]));
    if sequence && let Some(i) = V1_FIELDS.iter().copied().position(|n| n == field) {
        return format!("/{i}{tail}");
    }
    path.into()
}

// Only syntactic carrier locations are marked Deferred; this never selects a
// schema or claims payload validity. Numeric /5 is the V1 candidate carrier even
// when an earlier duplicate or malformed version prevents semantic dispatch.
// JsonAddress steps preserve repeated ancestors and descendant occurrences.
pub(super) fn deferred_observation_source(source: &SourceRef) -> bool {
    let SourceLocation::Cell {
        column_name,
        json: Some(a),
        ..
    } = &source.location
    else {
        return false;
    };
    if column_name != "handle_state_json" {
        return false;
    }
    let member = |i: usize, expected: &str| matches!(a.0.get(i), Some(JsonStep::Member { name, .. }) if name == expected);
    let pending = member(0, "pending_queue")
        || matches!(a.0.first(), Some(JsonStep::Index(i)) if Some(*i) == V1_FIELDS.iter().copied().position(|n| n == "pending_queue"));
    pending && matches!(a.0.get(1), Some(JsonStep::Index(_)))
        || member(0, "canonical_inputs")
            // ConversationInputCheckpoint (checkpoint.rs:103) and each released
            // CheckpointV2/V3/V4 declare pending second. No released leaf decoding.
            && (member(1, "pending") || matches!(a.0.get(1), Some(JsonStep::Index(1))))
            && matches!(a.0.get(2), Some(JsonStep::Index(_)))
            // A tag-first System sequence carries its observation at index 2.
            // This remains a syntactic candidate, never a variant acceptance.
            && (member(3, "observation") || matches!(a.0.get(3), Some(JsonStep::Index(2))))
}
