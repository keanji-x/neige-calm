//! `calm.track.rename`, the planner agent's naming write. Name-once: succeeds only while the track's title is empty;
//! refusals are values (`{"ok": false, "refused": …}`), not errors, and the write is attributed to the planner session, never the user.

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

/// Carries an in-tx refusal through `CalmError::Conflict`; no row writer's conflict message starts with this marker.
const REFUSED_MARKER: &str = "calm.track.rename refused: ";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(track_rename_descriptor(), wrap(track_rename));
}

fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture {
        let result = f(ctx, identity, args);
        Box::pin(async move {
            result
                .await
                .map(crate::mcp_server::result::ToolResult::structured)
        })
    })
}

fn track_rename_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TRACK_RENAME.into(),
        description: include_str!("../../../prompts/tools/calm.track.rename.md")
            .trim_end()
            .to_string(),
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
                // The only gate. `BEGIN IMMEDIATE` already holds the writer lock here, so a second concurrent rename reads the title the first one committed.
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

/// JSON payload so the title survives the round trip through `CalmError::Conflict`'s `String` intact.
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
