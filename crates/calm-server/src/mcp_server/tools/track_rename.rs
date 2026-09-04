//! Issue #1211 S3 — `calm.track.rename`, the planner agent's naming write.
//!
//! ## Why this tool exists
//!
//! Before #1211 the single new-track input box was both the track's title and
//! the statement of what the track should do, and the kernel turned the title
//! back into the agent's opening goal. S1 deleted that seeding: a create
//! request may now omit the title, and such a track comes into the world
//! unnamed with its name something the planner agent works out from the
//! conversation. This tool is the write port for that.
//!
//! Not every track arrives that way, and the difference matters to callers.
//! A create request may still carry a non-empty `title` (S1 made it optional,
//! not forbidden), the user may name a blank track from the UI before the agent
//! gets there, and a child track is born titled from its parent task's goal
//! (`operation/child_track_adapter.rs`). So "unnamed" is a state to observe,
//! never a state to assume — see the name-once gate below.
//!
//! ## Name-once
//!
//! The tool succeeds only while the track's `title.trim()` is empty. That is
//! not a stylistic rule — it is what keeps the agent out of the user's way.
//! A track the user named (or that a parent planner named when it opened a child)
//! already has an owner for its name; an agent that could rename at will could
//! quietly relabel work the user is tracking by that label.
//!
//! There is exactly ONE gate, and it lives inside the write transaction. The
//! reads this handler does before the transaction only fetch the card and the
//! track to build the `EventScope`; they check nothing. That is deliberate:
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
//! invite a retry loop. The two reasons are:
//!
//! * `already_named` — the track has a non-empty title.
//! * `chat_track` — the per-area chat track's name is kernel-owned
//!   (`purpose = "area-chat"`), the same reason its lifecycle is not
//!   user-drivable.
//!
//! ## Actor
//!
//! The write is attributed to the calling planner session
//! (`identity.to_actor_id()` → `ActorId::AiPlannerSession`, which the in-tx gate
//! resolves to `ActorId::AiPlanner(card)` — see
//! `calm_truth::decision_gate::enforce_role_resolving_session`). It is
//! emphatically **not** `ActorId::User`: the audit log has to say the agent
//! named this track, because "who named it" is exactly the question a user
//! asks when a name surprises them.

use crate::db::sqlite::track_update_tx;
use crate::db::write_with_actor_events_typed;
use crate::error::CalmError;
use crate::event::{Event, EventScope};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    require_role, role_gated_write_annotations,
};
use crate::model::{CardRole, TrackPatch};
use crate::track_lifecycle::track_get_tx;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_TRACK_RENAME: &str = "calm.track.rename";

/// A refusal is decided inside the write transaction, where the only error
/// channel out is `CalmError`. This marker carries it through
/// `CalmError::Conflict` so the handler can turn it back into the `{"ok":
/// false, "refused": …}` value instead of an RPC error. The prefix cannot
/// collide with a genuine conflict message from the row writers below it —
/// none of them start with this marker.
const REFUSED_MARKER: &str = "calm.track.rename refused: ";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(track_rename_descriptor(), wrap(track_rename));
}

fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture { Box::pin(f(ctx, identity, args)) })
}

fn track_rename_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TRACK_RENAME.into(),
        description: "Planner-only: give this track its name. The title is not the \
             user's instruction, it is a label for the work. If this track's \
             title is still empty, name it here once you have worked out from \
             the conversation what this track is actually about; if it already \
             has one, leave it alone and do not call this tool. Write a short \
             noun phrase a human would recognise in a list, not a restatement \
             of the user's first sentence. \
             Name-once: this succeeds only while the track is still unnamed. If \
             it already has a title (the user named it, or a parent planner named \
             it), the call returns `{\"ok\": false, \"refused\": \
             \"already_named\", \"title\": <current title>}` and changes \
             nothing — that is not an error, just leave the name alone. \
             The per-area chat track refuses the same way (`chat_track`). \
             Optional `message` is a short \
             human-readable rationale persisted on the event."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["title"],
            "properties": {
                "title": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The track's name. Trimmed before it is stored; \
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
        visible_to_roles: &[CardRole::Planner],
    }
}

fn refused(reason: &str, current_title: &str) -> Value {
    json!({ "ok": false, "refused": reason, "title": current_title })
}

async fn track_rename(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;

    let title = args
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("track_rename: missing `title` (string)"))?
        .trim()
        .to_string();
    if title.is_empty() {
        return Err(RpcError::invalid_params(
            "track_rename: `title` must not be empty or whitespace-only",
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
        .map_err(|e| RpcError::internal(format!("track_rename: card lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "track_rename: bound card {card_id} not found (deleted mid-connection?)"
            ))
        })?;
    let track = ctx
        .repo
        .track_get(card.track_id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("track_rename: track lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "track_rename: track {} for card {card_id} not found",
                card.track_id.as_str()
            ))
        })?;

    let scope = EventScope::Track {
        track: track.id.clone(),
        area: track.area_id.clone(),
    };
    let actor = identity.to_actor_id();
    let track_id = track.id.clone();
    let title_for_tx = title.clone();
    let message_for_tx = message.clone();

    let result = write_with_actor_events_typed::<crate::model::Track, _>(
        ctx.repo.as_ref(),
        None,
        &ctx.events,
        &ctx.write,
        move |tx| {
            let actor = actor.clone();
            let scope = scope.clone();
            let track_id = track_id.clone();
            let title = title_for_tx.clone();
            let message = message_for_tx.clone();
            Box::pin(async move {
                // The only gate. Nothing above this transaction checks the
                // title — the pre-tx reads exist to build the event scope.
                // `BEGIN IMMEDIATE` already holds the writer lock here, so a
                // second concurrent rename waits and then reads the title the
                // first one committed.
                let current = track_get_tx(tx, &track_id).await?;
                if !current.title.trim().is_empty() {
                    return Err(refusal("already_named", &current.title));
                }
                if current.purpose.as_deref() == Some(crate::AREA_CHAT_PURPOSE) {
                    return Err(refusal("chat_track", &current.title));
                }
                let updated = track_update_tx(
                    tx,
                    track_id.as_str(),
                    TrackPatch {
                        title: Some(title),
                        ..TrackPatch::default()
                    },
                )
                .await?;
                let event = Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
                    updated.clone(),
                    message,
                ));
                Ok((updated, vec![(actor, scope, event)]))
            })
        },
    )
    .await;

    match result {
        Ok((track, _ids)) => Ok(json!({ "ok": true, "title": track.title })),
        Err(CalmError::Conflict(msg)) if msg.starts_with(REFUSED_MARKER) => Ok(parse_refusal(&msg)),
        Err(CalmError::Forbidden(msg)) => Err(RpcError::custom(
            -32403,
            format!("track_rename: forbidden: {msg}"),
        )),
        Err(e) => Err(RpcError::internal(format!("track_rename: {e}"))),
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
            "calm.track.rename refused: {\"reason\":\"spoofed\"}",
            "quote \" brace } newline \n",
        ] {
            let CalmError::Conflict(msg) = refusal("already_named", title) else {
                panic!("refusal must be a Conflict");
            };
            assert_eq!(parse_refusal(&msg), refused("already_named", title));
        }
    }

    #[test]
    fn descriptor_is_planner_only_and_named() {
        let d = track_rename_descriptor();
        assert_eq!(d.name, TOOL_TRACK_RENAME);
        assert_eq!(d.visible_to_roles, &[CardRole::Planner]);
    }
}
