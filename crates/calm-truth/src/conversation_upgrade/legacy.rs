//! Strict V1 source facts, without the runtime's padding or defaulting decoder.
use super::FactCoverageError;
use super::classify::*;
use calm_types::planner_attachment::BoundAttachment;
use serde_json::Value;
use std::collections::HashMap;

pub(super) const PARALLEL: [&str; 4] = [
    "pending_queue",
    "pending_envelope_ids",
    "pending_entry_meta",
    "pending_message_ids",
];
pub(super) const HEADER: [&str; 20] = [
    "schema_version",
    "mode",
    "phase",
    "turn_budget",
    "push_watermark",
    "last_thread_id",
    "last_turn_id",
    "last_report_body_sha256",
    "last_seen_head",
    "issued_turn_head",
    "wedged_reason",
    "interruption_intent",
    "token_usage",
    "compact_outcome",
    "planner_compaction",
    "context_usage_invalidated",
    "context_usage_invalidated_turn",
    "pending_rewind",
    "last_turn_base",
    "projection_client_id",
];
pub(super) fn facts(f: &mut FrameView<'_>) -> Result<(), FactCoverageError> {
    super::nested::header(f)?;
    if let Some(a) = f.value.as_array() {
        for i in super::root::V1_FIELDS.len()..a.len() {
            f.block(&format!("/{i}"), FactReason::UnknownField)?;
        }
    }
    let max = PARALLEL
        .iter()
        .filter_map(|n| f.value_at(&format!("/{n}"))?.as_array().map(Vec::len))
        .max()
        .unwrap_or(0);
    let lengths: Vec<_> = PARALLEL
        .iter()
        .map(|n| {
            f.value_at(&format!("/{n}"))
                .and_then(Value::as_array)
                .map(Vec::len)
        })
        .collect();
    let mut entry_ids = HashMap::<String, String>::new();
    let mut message_ids = HashMap::<String, String>::new();
    for name in PARALLEL {
        let path = format!("/{name}");
        match f.value_at(&path).and_then(Value::as_array) {
            Some(a) => f.record(&path, Interpretation::ArrayLength(a.len()))?,
            None => f.check::<Vec<Value>>(&path)?,
        }
        if lengths.windows(2).any(|p| p[0] != p[1]) {
            f.block(&path, FactReason::UnequalLengths)?;
        }
        for i in 0..max {
            let slot = format!("{path}/{i}");
            match name {
                "pending_queue" => super::nested::observation(f, &slot, false)?,
                "pending_envelope_ids" => f.check::<i64>(&slot)?,
                "pending_entry_meta" => {
                    match f.value_at(&slot) {
                        None => f.block(&slot, FactReason::Missing)?,
                        Some(Value::Null) => f.block(&slot, FactReason::Null)?,
                        Some(v) if !v.is_object() && !v.is_array() => {
                            f.block(&slot, FactReason::Malformed)?
                        }
                        _ => {}
                    }
                    // server::harness::snapshot::QueueEntryMeta declaration;
                    // no Value success stands in for the metadata owner.
                    for (name, p) in
                        f.structure(&slot, "id rev queued_at_ms attachments", "attachments")?
                    {
                        match name {
                            "id" => identity(f, &p, &mut entry_ids)?,
                            "rev" => f.check::<u32>(&p)?,
                            "queued_at_ms" => f.check::<i64>(&p)?,
                            _ => {
                                f.check::<Vec<BoundAttachment>>(&p)?;
                                for a in f.slots(&p) {
                                    f.check::<BoundAttachment>(&a)?;
                                    super::nested::attachment(f, &a, true)?;
                                }
                            }
                        }
                    }
                }
                "pending_message_ids" => {
                    f.check::<Vec<String>>(&slot)?;
                    if let Some(a) = f.value_at(&slot).and_then(Value::as_array) {
                        for j in 0..a.len() {
                            identity(f, &format!("{slot}/{j}"), &mut message_ids)?;
                        }
                    }
                }
                _ => unreachable!(),
            }
        }
    }
    super::nested::issued(f)?;
    Ok(())
}
fn identity(
    f: &mut FrameView<'_>,
    path: &str,
    seen: &mut HashMap<String, String>,
) -> Result<(), FactCoverageError> {
    f.check::<String>(path)?;
    if let Some(id) = f.value_at(path).and_then(Value::as_str) {
        if id.is_empty() {
            f.block(path, FactReason::Malformed)?;
        } else if let Some(first) = seen.insert(id.into(), path.into()) {
            f.block(&first, FactReason::DuplicateIdentity)?;
            f.block(path, FactReason::DuplicateIdentity)?;
        }
    }
    Ok(())
}
