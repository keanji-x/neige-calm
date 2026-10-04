//! `calm.ratify.request`: a Planner asks the user to ratify a decision on an open Track.

use crate::db::write_with_actor_events_typed;
use crate::error::CalmError;
use crate::event::{Event, EventScope};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    require_role, role_gated_write_annotations,
};
use crate::model::{CardRole, Track};
use crate::ratify_state::ratify_request_pending_tx;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_RATIFY_REQUEST: &str = "calm.ratify.request";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(ratify_request_descriptor(), wrap(ratify_request));
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

fn ratify_request_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_RATIFY_REQUEST.into(),
        description: include_str!("../../../prompts/tools/calm.ratify.request.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["reason"],
            "properties": {
                "reason": { "type": "string", "minLength": 1 }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Planner],
    }
}

#[derive(Clone, Debug, Deserialize)]
struct RatifyRequestArgs {
    reason: String,
}

async fn ratify_request(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    let args: RatifyRequestArgs = serde_json::from_value(args)
        .map_err(|e| RpcError::invalid_params(format!("ratify_request: invalid args: {e}")))?;
    if args.reason.trim().is_empty() {
        return Err(RpcError::invalid_params(
            "ratify_request: reason must not be empty",
        ));
    }

    let (_card, track) = resolve_track_for_identity(&ctx, &identity).await?;
    let actor = identity.to_actor_id();
    let scope = EventScope::Track {
        track: track.id.clone(),
        area: track.area_id.clone(),
    };
    let track_id = track.id.clone();
    let reason = args.reason;

    let result =
        write_with_actor_events_typed::<(), _>(ctx.repo.as_ref(), None, &ctx.events, &ctx.write, {
            move |tx| {
                let actor = actor.clone();
                let scope = scope.clone();
                let track_id = track_id.clone();
                let reason = reason.clone();
                Box::pin(async move {
                    if !crate::db::sqlite::track_get_tx(tx, &track_id)
                        .await?
                        .is_open()
                    {
                        return Err(CalmError::BadRequest(
                            "ratify_request: the track is closed; only the user reopens it".into(),
                        ));
                    }
                    if ratify_request_pending_tx(tx, &track_id).await? {
                        return Err(CalmError::BadRequest(
                            "ratify_request: a ratify request is already pending; wait for the \
                             user's grant or deny"
                                .into(),
                        ));
                    }
                    Ok((
                        (),
                        vec![(actor, scope, Event::RatifyRequested { track_id, reason })],
                    ))
                })
            }
        })
        .await;

    match result {
        Ok((_unit, _ids)) => Ok(json!({ "ok": true })),
        Err(CalmError::BadRequest(msg)) => Err(RpcError::invalid_params(msg)),
        Err(CalmError::Forbidden(msg)) => Err(RpcError::custom(
            -32403,
            format!("ratify_request: forbidden: {msg}"),
        )),
        Err(e) => Err(RpcError::internal(format!("ratify_request: {e}"))),
    }
}

async fn resolve_track_for_identity(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
) -> Result<(crate::model::Card, Track), RpcError> {
    let card_id_str = identity.card_id.as_str().to_string();
    let card = ctx
        .repo
        .card_get(&card_id_str)
        .await
        .map_err(|e| RpcError::internal(format!("review: card lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "review: bound card {card_id_str} not found (deleted mid-connection?)"
            ))
        })?;
    let track = ctx
        .repo
        .track_get(card.track_id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("review: track lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "review: track {} for card {} not found",
                card.track_id.as_str(),
                card_id_str
            ))
        })?;
    Ok((card, track))
}
