//! Presence annotations for declared fields; leaf acceptance remains with owners.
use super::{FactCoverageError, classify::*};
use calm_types::{
    harness::{budget::*, snapshot::*, *},
    model::*,
};
use serde_json::Value;

impl FrameView<'_> {
    fn presence(&mut self, path: &str, required: bool) -> Result<(), FactCoverageError> {
        let presence = match self.value_at(path) {
            None => Presence::Missing,
            Some(Value::Null) => Presence::Null,
            Some(_) => Presence::Present,
        };
        match presence {
            Presence::Missing => self.block(path, FactReason::Missing)?,
            Presence::Null => self.block(path, FactReason::Null)?,
            Presence::Present => {}
        }
        self.record(path, Interpretation::Presence { required, presence })
    }
    // Declaration order is owner-defined; all addresses refer to original nodes.
    pub fn structure<'a>(
        &mut self,
        path: &str,
        declaration: &'a str,
        optional: &str,
    ) -> Result<Vec<(&'a str, String)>, FactCoverageError> {
        let Some(value) = self.value_at(path) else {
            return Ok(Vec::new());
        };
        if !value.is_object() && !value.is_array() {
            if value.is_null() {
                self.block(path, FactReason::Null)?;
            }
            return Ok(Vec::new());
        }
        let fields: Vec<_> = declaration.split_ascii_whitespace().collect();
        let mut paths = Vec::new();
        for (i, name) in fields.iter().enumerate() {
            let key = if value.is_array() {
                i.to_string()
            } else {
                (*name).into()
            };
            let p = format!("{path}/{key}");
            self.presence(&p, !optional.split_ascii_whitespace().any(|n| n == *name))?;
            if value.is_array() {
                self.worker
                    .sequence_fields
                    .push((format!("{path}/{name}"), p.clone()));
            }
            paths.push((*name, p));
        }
        let unknown: Vec<_> = match value {
            Value::Array(a) => (fields.len()..a.len()).map(|i| i.to_string()).collect(),
            Value::Object(o) => o
                .keys()
                .filter(|n| !fields.contains(&n.as_str()))
                .map(|n| n.replace('~', "~0").replace('/', "~1"))
                .collect(),
            _ => unreachable!(),
        };
        for key in unknown {
            self.block(&format!("{path}/{key}"), FactReason::UnknownField)?;
        }
        Ok(paths)
    }
    fn tag(&self, path: &str, name: &str) -> Option<&str> {
        let value = self.value_at(path)?;
        value
            .get(name)
            .or_else(|| value.as_array()?.first())?
            .as_str()
    }
    pub fn slots(&self, path: &str) -> Vec<String> {
        (0..self
            .value_at(path)
            .and_then(Value::as_array)
            .map_or(0, Vec::len))
            .map(|i| format!("{path}/{i}"))
            .collect()
    }
}
pub(super) fn header(f: &mut FrameView<'_>) -> Result<(), FactCoverageError> {
    header_for(f, false)
}
pub(super) fn released_header(f: &mut FrameView<'_>) -> Result<(), FactCoverageError> {
    header_for(f, true)
}
fn header_for(f: &mut FrameView<'_>, released: bool) -> Result<(), FactCoverageError> {
    for name in super::legacy::HEADER
        .into_iter()
        .filter(|name| !released || !matches!(*name, "planner_compaction" | "projection_client_id"))
    {
        let path = format!("/{name}");
        match name {
            "schema_version" => f.check::<u32>(&path)?,
            "mode" => {
                f.check::<String>(&path)?;
                if f.value_at(&path).and_then(Value::as_str)
                    != Some(calm_types::harness::HARNESS_MODE)
                {
                    f.block(&path, FactReason::Malformed)?;
                }
            }
            "phase" => f.check::<HarnessPhaseTag>(&path)?,
            "turn_budget" => f.check::<FrozenBudget>(&path)?,
            "push_watermark" => f.check::<i64>(&path)?,
            "context_usage_invalidated" => f.check::<bool>(&path)?,
            "interruption_intent" => f.check::<HarnessInterruptionIntent>(&path)?,
            "token_usage" => f.check::<TokenUsage>(&path)?,
            "compact_outcome" => f.check::<CompactOutcome>(&path)?,
            "pending_rewind" => f.check::<PersistedRewind>(&path)?,
            "last_turn_base" => f.check::<TurnBase>(&path)?,
            // Its server owner has no public IO-free strict decoder. Preserve all
            // descendants with the exact source; do not duplicate that policy.
            "planner_compaction" => {
                f.check::<Value>(&path)?;
                f.block(&path, FactReason::MissingOwnerDecoder)?;
            }
            _ => f.check::<String>(&path)?,
        }
    }
    let planner = f.tag("/turn_budget", "kind") == Some("planner");
    let rewind = match f.tag("/pending_rewind", "provider") {
        Some("codex") => ("provider before_turn_id reverted", "reverted"),
        Some("codex_v2") => ("provider thread_id before_turn_id reverted", ""),
        Some("claude") => ("provider resume_at drops_turn", ""),
        _ => ("provider", ""),
    };
    let intent = intent_layout(f)?;
    // Public declarations fix positions; tagged enums fix their discriminator.
    for (parent, declaration, optional) in [
        (
            "/token_usage",
            "used_tokens total_tokens context_window at_ms",
            "context_window",
        ),
        (
            "/compact_outcome",
            "operation_id thread_id turn_id status",
            "turn_id",
        ),
        ("/last_turn_base", "turn_id seen_head", "seen_head"),
        (
            "/turn_budget",
            if planner {
                "kind budget start_attempt warning_attempted context_usage"
            } else {
                "kind"
            },
            "context_usage",
        ),
        ("/pending_rewind", rewind.0, rewind.1),
        ("/interruption_intent", intent, ""),
    ] {
        if declaration.is_empty() {
            continue;
        }
        for (name, p) in f.structure(parent, declaration, optional)? {
            match name {
                "used_tokens" | "total_tokens" | "context_window" | "at_ms" | "requested_at_ms" => {
                    f.check::<i64>(&p)?
                }
                "effective_elapsed_ms" | "budget_ms" => f.check::<u64>(&p)?,
                "status" => f.check::<CompactStatus>(&p)?,
                "reason" => f.check::<HarnessInterruptionReason>(&p)?,
                "budget" => f.check::<PlannerTurnBudget>(&p)?,
                "start_attempt" => f.check::<QueueEntryId>(&p)?,
                "warning_attempted" | "reverted" => f.check::<bool>(&p)?,
                "resume_at" | "drops_turn" => f.check::<uuid::Uuid>(&p)?,
                "context_usage" => {
                    f.check::<FrozenContextUsage>(&p)?;
                    let tag = f.tag(&p, "kind");
                    let text = tag == Some("text");
                    let unit = matches!(tag, Some("render_plan" | "legacy_not_recorded"));
                    let content = text
                        || f.value_at(&p).is_some_and(|v| {
                            v.as_array().is_some_and(|a| a.len() > 1) || v.get("text").is_some()
                        });
                    for (name, leaf) in
                        f.structure(&p, if content { "kind text" } else { "kind" }, "")?
                    {
                        match name {
                            "text" if text => f.check::<String>(&leaf)?,
                            "text" if unit => f.check::<()>(&leaf)?,
                            "text" => ambiguous(f, &leaf)?,
                            _ => f.check::<String>(&leaf)?,
                        }
                    }
                }
                _ => f.check::<String>(&p)?,
            }
        }
    }
    Ok(())
}
// Unresolved ownership annotates existing sources only, never inferred fields.
fn ambiguous(f: &mut FrameView<'_>, path: &str) -> Result<(), FactCoverageError> {
    let path = f.path(path);
    let paths: Vec<_> = f
        .worker
        .fields
        .iter()
        .filter_map(|field| {
            if field.interpretation == Interpretation::Blocked(FactReason::Unclassified)
                && same_cell(&field.source, &f.cell)
                && let super::SourceLocation::Cell { json: Some(a), .. } = &field.source.location
                && (a.pointer() == path || a.pointer().starts_with(&format!("{path}/")))
            {
                Some(a.pointer())
            } else {
                None
            }
        })
        .collect();
    for p in paths {
        f.block(&p, FactReason::AmbiguousRepresentation)?;
    }
    Ok(())
}
fn intent_layout(f: &mut FrameView<'_>) -> Result<&'static str, FactCoverageError> {
    let path = "/interruption_intent";
    let Some(v) = f.value_at(path).filter(|v| v.is_object() || v.is_array()) else {
        return Ok("");
    };
    let owner = serde_path_to_error::deserialize::<_, HarnessInterruptionIntent>(v);
    let tag = f.tag(path, "version");
    if v.is_object() && v.get("version").is_some() && !matches!(tag, Some("v1" | "user_stop_v1")) {
        f.block(&format!("{path}/version"), FactReason::UnsupportedVersion)?;
        return Ok("version");
    }
    let selected = match owner {
        Ok(HarnessInterruptionIntent::Legacy(_)) => None,
        Ok(HarnessInterruptionIntent::Versioned(_)) => Some("v1"),
        Ok(HarnessInterruptionIntent::Control(_)) => Some("user_stop_v1"),
        // Legacy denies extra fields and has exactly two sequence positions.
        // A version member excludes Legacy maps. A non-version tag excludes
        // both tagged owners. Short colliding sequences prove neither layout.
        Err(_)
            if v.is_object()
                || !matches!(tag, Some("v1" | "user_stop_v1"))
                    && !v.as_array().is_some_and(Vec::is_empty)
                || v.as_array().is_some_and(|a| a.len() > 2) =>
        {
            tag
        }
        Err(_) => {
            ambiguous(f, path)?;
            // All three public declarations require an initial string slot.
            if v.as_array().is_some_and(Vec::is_empty) {
                f.check::<String>(&format!("{path}/0"))?;
            }
            return Ok("");
        }
    };
    Ok(match selected {
        Some("v1") => {
            "version session_id card_id track_id thread_id turn_id reason requested_at_ms effective_elapsed_ms budget_ms"
        }
        Some("user_stop_v1") => {
            "version session_id card_id track_id thread_id turn_id requested_at_ms"
        }
        Some(_) if v.is_object() && v.get("version").is_some() => "version",
        _ => "turn_id reason",
    })
}

pub(super) fn issued(f: &mut FrameView<'_>) -> Result<(), FactCoverageError> {
    // server::harness::snapshot::IssuedInputSegments has two required fields.
    let root = "/issued_input_segments";
    let mut accepted = f
        .value_at(root)
        .is_some_and(|v| v.is_object() || v.as_array().is_some_and(|a| a.len() == 2));
    let mut segments = Vec::new();
    for (name, p) in f.structure(root, "turn_id segments", "")? {
        if name == "turn_id" {
            f.check::<String>(&p)?;
        } else {
            f.check::<Vec<HarnessInputSegment>>(&p)?;
            segments = f.slots(&p);
        }
        accepted &= f.worker.fields.iter().any(|field| {
            Some(&field.source) == f.source_at(&p).as_ref()
                && matches!(field.interpretation, Interpretation::Stored(_))
        });
    }
    match f.value_at(root) {
        None => f.block(root, FactReason::Missing)?,
        Some(Value::Null) => f.block(root, FactReason::Null)?,
        Some(v) if accepted => f.record(root, Interpretation::Stored(v.clone()))?,
        _ => f.block(root, FactReason::Malformed)?,
    }
    for s in segments {
        f.check::<calm_types::model::HarnessInputSegment>(&s)?;
        for (name, p) in f.structure(
            &s,
            "presentation text attachments origin",
            "attachments origin",
        )? {
            match name {
                "presentation" => f.check::<HarnessInputPresentation>(&p)?,
                "text" => f.check::<String>(&p)?,
                "attachments" => {
                    f.check::<Vec<calm_types::planner_attachment::PlannerAttachment>>(&p)?;
                    for a in f.slots(&p) {
                        f.check::<calm_types::planner_attachment::PlannerAttachment>(&a)?;
                        attachment(f, &a, false)?;
                    }
                }
                "origin" => {
                    f.check::<HarnessInputOrigin>(&p)?;
                    for (name, p) in f.structure(
                        &p,
                        "observation event_id queue_entry",
                        "event_id queue_entry",
                    )? {
                        match name {
                            "observation" => f.check::<String>(&p)?,
                            "event_id" => f.check::<i64>(&p)?,
                            _ => {
                                f.check::<HarnessQueueEntryRef>(&p)?;
                                for (name, p) in f.structure(&p, "id rev", "")? {
                                    if name == "id" {
                                        f.check::<String>(&p)?;
                                    } else {
                                        f.check::<u32>(&p)?;
                                    }
                                }
                            }
                        }
                    }
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(())
}
// Aggregate owners remain authoritative; each sibling leaf is checked even
// when the aggregate stopped at an earlier invalid id.
pub(super) fn attachment(
    f: &mut FrameView<'_>,
    path: &str,
    bound: bool,
) -> Result<(), FactCoverageError> {
    use calm_types::planner_attachment::AttachmentId;
    // planner_attachment public declarations differ in order.
    for (name, p) in f.structure(
        path,
        if bound {
            "id size path"
        } else {
            "id contentType size url"
        },
        "",
    )? {
        match name {
            "id" => f.check::<AttachmentId>(&p)?,
            "size" => f.check::<u64>(&p)?,
            _ => f.check::<String>(&p)?,
        }
    }
    Ok(())
}
pub(super) fn provider(f: &mut FrameView<'_>, text: &str) -> Result<(), FactCoverageError> {
    use crate::db::sqlite::provider_execution::*;
    f.provider_check::<Record>("", Some(text))?;
    for (name, p) in f.structure("", "version execution_id purpose release decision", "")? {
        match name {
            "version" => {
                f.check::<Version>(&p)?;
                if f.value_at(&p)
                    .and_then(Value::as_u64)
                    .is_some_and(|v| v > 1)
                {
                    f.block(&p, FactReason::UnsupportedVersion)?;
                }
            }
            "execution_id" => f.check::<String>(&p)?,
            "purpose" | "release" | "decision" => {
                let (variants, declaration) = match name {
                    "purpose" => {
                        f.provider_check::<Purpose>(&p, None)?;
                        (&["Turn", "Compaction"][..], "turn_id")
                    }
                    "release" => {
                        f.provider_check::<Release>(&p, None)?;
                        (&["Releasing", "Released"][..], "authorized_at_ms")
                    }
                    _ => {
                        f.provider_check::<Decision>(&p, None)?;
                        (&["Stopping"][..], "reason at_ms")
                    }
                };
                for variant in variants {
                    let variant = format!("{p}/{variant}");
                    if f.value_at(&variant)
                        .is_some_and(|v| v.is_object() || v.is_array())
                    {
                        for (name, leaf) in f.structure(&variant, declaration, "")? {
                            match name {
                                "reason" => f.check::<StopReason>(&leaf)?,
                                "turn_id" => f.check::<String>(&leaf)?,
                                _ => f.check::<i64>(&leaf)?,
                            }
                        }
                    }
                }
            }
            _ => unreachable!(),
        }
    }
    Ok(())
}
