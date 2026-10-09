//! `neige_worker_report` (#2492): the worker watcher's one report on a quiet task worker. The
//! argument shapes are checked here; the caller check, the same-Track resolution and the
//! kernel-written wake are [`crate::worker_watch::report`]'s.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    role_gated_write_annotations,
};
use crate::mcp_server::result::ToolResult;
use crate::model::CardRole;
use crate::worker_watch::{MAX_NOTE_CHARS, Note, Outcome, Verdict};

pub const TOOL_WORKER_REPORT: &str = "neige_worker_report";

pub fn register_into(registry: &mut ToolRegistry) {
    let handler: ToolHandler = Arc::new(|ctx, identity, args| -> ToolHandlerFuture {
        Box::pin(async move { worker_report(ctx, identity, args).await })
    });
    registry.register(descriptor(), handler);
}

fn descriptor() -> ToolDescriptor {
    let outcomes: Vec<&str> = Outcome::ALL
        .iter()
        .map(|outcome| outcome.as_str())
        .collect();
    ToolDescriptor {
        name: TOOL_WORKER_REPORT.into(),
        description: include_str!("../../../prompts/tools/neige_worker_report.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["attempt_id", "outcome"],
            "additionalProperties": false,
            "properties": {
                "attempt_id": { "type": "string", "minLength": 1 },
                "outcome": { "type": "string", "enum": outcomes },
                "note": { "type": "string", "minLength": 1, "maxLength": MAX_NOTE_CHARS }
            }
        }),
        // The kernel writes only a wake for the caller's own Track's Planner.
        annotations: Some(role_gated_write_annotations()),
        roles: &[CardRole::Assistant],
        listed_for: &[CardRole::Assistant],
    }
}

/// The arguments as typed; the registry has already refused a key outside the schema.
#[derive(Deserialize)]
struct Args {
    attempt_id: String,
    outcome: Outcome,
    note: Option<String>,
}

fn verdict(args: Value) -> Result<(String, Verdict), RpcError> {
    let args: Args = serde_json::from_value(args).map_err(|error| {
        RpcError::invalid_params(format!(
            "{error}; outcome is one of {}",
            Outcome::ALL.map(Outcome::as_str).join(", ")
        ))
    })?;
    if args.attempt_id.is_empty() {
        return Err(RpcError::invalid_params("attempt_id must not be empty"));
    }
    let note = args
        .note
        .as_deref()
        .map(Note::parse)
        .transpose()
        .map_err(RpcError::invalid_params)?;
    let verdict = Verdict::new(args.outcome, note).ok_or_else(|| {
        RpcError::invalid_params("outcome needs_owner needs a note: one sentence saying what")
    })?;
    Ok((args.attempt_id, verdict))
}

async fn worker_report(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<ToolResult, RpcError> {
    let (attempt_id, verdict) = verdict(args)?;
    let reported = crate::worker_watch::report(&ctx, &identity, &attempt_id, &verdict).await?;
    Ok(ToolResult::structured(
        json!({ "key": reported.key, "replayed": reported.replayed }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_outcome_is_a_closed_set_and_needs_owner_needs_a_note() {
        for outcome in Outcome::ALL {
            let (_, verdict) =
                verdict(json!({"attempt_id":"t:a","outcome":outcome.as_str(),"note":"why"}))
                    .unwrap();
            assert_eq!(verdict.outcome(), outcome);
        }
        let refused = verdict(json!({"attempt_id":"t:a","outcome":"done"})).unwrap_err();
        assert_eq!(refused.code, RpcError::INVALID_PARAMS);
        assert!(
            refused
                .message
                .contains("trust_accepted, idle_at_prompt, needs_owner, unclear"),
            "{}",
            refused.message
        );
        let missing = verdict(json!({"attempt_id":"t:a","outcome":"needs_owner"})).unwrap_err();
        assert_eq!(missing.code, RpcError::INVALID_PARAMS);
        assert!(
            missing.message.contains("needs a note"),
            "{}",
            missing.message
        );
        for note in ["", "  ", "two\nlines"] {
            let bad = verdict(json!({"attempt_id":"t:a","outcome":"unclear","note":note}));
            assert!(bad.is_err(), "{note:?}");
        }
        let (_, unnoted) = verdict(json!({"attempt_id":"t:a","outcome":"unclear"})).unwrap();
        assert_eq!(unnoted, Verdict::Unclear(None));
    }

    #[test]
    fn the_schema_enum_is_the_outcome_set() {
        let descriptor = descriptor();
        assert_eq!(
            descriptor.input_schema["properties"]["outcome"]["enum"],
            json!(["trust_accepted", "idle_at_prompt", "needs_owner", "unclear"])
        );
        assert_eq!(descriptor.roles, &[CardRole::Assistant]);
    }
}
