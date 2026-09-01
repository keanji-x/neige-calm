//! Issue #1211 S3 — `calm.wave.rename`, the spec agent's naming write.
//!
//! ## Why this tool exists
//!
//! Before #1211 the single new-wave input box was both the wave's title and
//! the statement of what the wave should do, and the kernel turned the title
//! back into the agent's opening goal. S1 deleted that seeding: a wave now
//! comes into the world unnamed, and the name is something the spec agent
//! works out from the conversation. This tool is the write port for that.
//!
//! ## Name-once
//!
//! The tool succeeds only while the wave's `title.trim()` is empty. That is
//! not a stylistic rule — it is what keeps the agent out of the user's way.
//! A wave the user named (or that a parent spec named when it opened a child)
//! already has an owner for its name; an agent that could rename at will could
//! quietly relabel work the user is tracking by that label.
//!
//! There is exactly ONE gate, and it lives inside the write transaction. The
//! reads this handler does before the transaction only fetch the card and the
//! wave to build the `EventScope`; they check nothing. That is deliberate:
//! `write_with_actor_events_typed` opens the tx with `BEGIN IMMEDIATE`, so the
//! writer lock is already held when the title is re-read, and two concurrent
//! renames serialise — the second one sees the non-empty title the first one
//! wrote and is refused. A pre-tx copy of the check would decide nothing and
//! would only be a second place for the rule to drift.
//!
//! ## Refusals are values, not errors
//!
//! Every refusal comes back as `{"ok": false, "refused": <reason>, "title":
//! <the title that is already there>}`. The right agent behaviour on refusal
//! is "leave it alone and get on with the work", so a refusal is ordinary
//! information, not a fault: a `-32603` would read as "the kernel broke" and
//! invite a retry loop. The three reasons are:
//!
//! * `already_named` — the wave has a non-empty title.
//! * `template_wave` — template waves are a catalogue the user curates; the
//!   names ARE the catalogue entries.
//! * `chat_wave` — the per-cove chat wave's name is kernel-owned
//!   (`purpose = "cove-chat"`), the same reason its lifecycle is not
//!   user-drivable.
//!
//! ## Actor
//!
//! The write is attributed to the calling spec session
//! (`identity.to_actor_id()` → `ActorId::AiSpecSession`, which the in-tx gate
//! resolves to `ActorId::AiSpec(card)` — see
//! `calm_truth::decision_gate::enforce_role_resolving_session`). It is
//! emphatically **not** `ActorId::User`: the audit log has to say the agent
//! named this wave, because "who named it" is exactly the question a user
//! asks when a name surprises them.

use crate::db::sqlite::{wave_has_template_overlay_tx, wave_update_tx};
use crate::db::write_with_actor_events_typed;
use crate::error::CalmError;
use crate::event::{Event, EventScope};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    require_role, role_gated_write_annotations,
};
use crate::model::{CardRole, WavePatch};
use crate::wave_lifecycle::wave_get_tx;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_WAVE_RENAME: &str = "calm.wave.rename";

/// A refusal is decided inside the write transaction, where the only error
/// channel out is `CalmError`. This marker carries it through
/// `CalmError::Conflict` so the handler can turn it back into the `{"ok":
/// false, "refused": …}` value instead of an RPC error. The prefix cannot
/// collide with a genuine conflict message from the row writers below it —
/// none of them start with this marker.
const REFUSED_MARKER: &str = "calm.wave.rename refused: ";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(wave_rename_descriptor(), wrap(wave_rename));
}

fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture { Box::pin(f(ctx, identity, args)) })
}

fn wave_rename_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_WAVE_RENAME.into(),
        description: "Spec-only: give this wave its name. A wave is created \
             unnamed — the title is not the user's instruction, it is a label \
             for the work — so once you have worked out from the conversation \
             what this track is actually about, name it here. Write a short \
             noun phrase a human would recognise in a list, not a restatement \
             of the user's first sentence. \
             Name-once: this succeeds only while the wave is still unnamed. If \
             it already has a title (the user named it, or a parent spec named \
             it), the call returns `{\"ok\": false, \"refused\": \
             \"already_named\", \"title\": <current title>}` and changes \
             nothing — that is not an error, just leave the name alone. \
             Template waves and the per-cove chat wave refuse the same way \
             (`template_wave` / `chat_wave`). Optional `message` is a short \
             human-readable rationale persisted on the event."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["title"],
            "properties": {
                "title": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The wave's name. Trimmed before it is stored; \
                                    whitespace-only is rejected."
                },
                "message": {
                    "type": "string",
                    "description": "Optional short rationale, persisted as the \
                                    event's agent_message."
                }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Spec],
    }
}

fn refused(reason: &str, current_title: &str) -> Value {
    json!({ "ok": false, "refused": reason, "title": current_title })
}

async fn wave_rename(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;

    let title = args
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("wave_rename: missing `title` (string)"))?
        .trim()
        .to_string();
    if title.is_empty() {
        return Err(RpcError::invalid_params(
            "wave_rename: `title` must not be empty or whitespace-only",
        ));
    }
    let message = args
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let card_id = identity.card_id.clone();
    let card = ctx
        .repo
        .card_get(&card_id)
        .await
        .map_err(|e| RpcError::internal(format!("wave_rename: card lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "wave_rename: bound card {card_id} not found (deleted mid-connection?)"
            ))
        })?;
    let wave = ctx
        .repo
        .wave_get(card.wave_id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("wave_rename: wave lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "wave_rename: wave {} for card {card_id} not found",
                card.wave_id.as_str()
            ))
        })?;

    let scope = EventScope::Wave {
        wave: wave.id.clone(),
        cove: wave.cove_id.clone(),
    };
    let actor = identity.to_actor_id();
    let wave_id = wave.id.clone();
    let title_for_tx = title.clone();
    let message_for_tx = message.clone();

    let result = write_with_actor_events_typed::<crate::model::Wave, _>(
        ctx.repo.as_ref(),
        None,
        &ctx.events,
        &ctx.write,
        move |tx| {
            let actor = actor.clone();
            let scope = scope.clone();
            let wave_id = wave_id.clone();
            let title = title_for_tx.clone();
            let message = message_for_tx.clone();
            Box::pin(async move {
                // The only gate. Nothing above this transaction checks the
                // title — the pre-tx reads exist to build the event scope.
                // `BEGIN IMMEDIATE` already holds the writer lock here, so a
                // second concurrent rename waits and then reads the title the
                // first one committed.
                let current = wave_get_tx(tx, &wave_id).await?;
                if !current.title.trim().is_empty() {
                    return Err(refusal("already_named", &current.title));
                }
                if current.purpose.as_deref() == Some(crate::COVE_CHAT_PURPOSE) {
                    return Err(refusal("chat_wave", &current.title));
                }
                if wave_has_template_overlay_tx(tx, wave_id.as_str()).await? {
                    return Err(refusal("template_wave", &current.title));
                }
                let updated = wave_update_tx(
                    tx,
                    wave_id.as_str(),
                    WavePatch {
                        title: Some(title),
                        ..WavePatch::default()
                    },
                )
                .await?;
                let event = Event::WaveUpdated(crate::event::WaveUpdatedPayload::new(
                    updated.clone(),
                    message,
                ));
                Ok((updated, vec![(actor, scope, event)]))
            })
        },
    )
    .await;

    match result {
        Ok((wave, _ids)) => Ok(json!({ "ok": true, "title": wave.title })),
        Err(CalmError::Conflict(msg)) if msg.starts_with(REFUSED_MARKER) => Ok(parse_refusal(&msg)),
        Err(CalmError::Forbidden(msg)) => Err(RpcError::custom(
            -32403,
            format!("wave_rename: forbidden: {msg}"),
        )),
        Err(e) => Err(RpcError::internal(format!("wave_rename: {e}"))),
    }
}

/// Pack a refusal into the transaction's error channel. The payload is JSON
/// so the title (which may contain anything) survives the round trip through
/// `CalmError::Conflict`'s `String` intact.
fn refusal(reason: &str, current_title: &str) -> CalmError {
    CalmError::Conflict(format!(
        "{REFUSED_MARKER}{}",
        json!({ "reason": reason, "title": current_title })
    ))
}

fn parse_refusal(msg: &str) -> Value {
    let body: Value = serde_json::from_str(&msg[REFUSED_MARKER.len()..]).unwrap_or(Value::Null);
    refused(
        body.get("reason")
            .and_then(Value::as_str)
            .unwrap_or("refused"),
        body.get("title")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_round_trips_reason_and_title_through_the_error_channel() {
        // The title travels through a `String` error, so a title that looks
        // like the marker itself, or that contains quotes and braces, must
        // still come out verbatim.
        for title in [
            "ordinary name",
            "calm.wave.rename refused: {\"reason\":\"spoofed\"}",
            "quote \" brace } newline \n",
        ] {
            let CalmError::Conflict(msg) = refusal("already_named", title) else {
                panic!("refusal must be a Conflict");
            };
            assert_eq!(parse_refusal(&msg), refused("already_named", title));
        }
    }

    #[test]
    fn descriptor_is_spec_only_and_named() {
        let d = wave_rename_descriptor();
        assert_eq!(d.name, TOOL_WAVE_RENAME);
        assert_eq!(d.visible_to_roles, &[CardRole::Spec]);
    }
}
