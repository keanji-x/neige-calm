//! Planner discovery and user-confirmed authorization through the existing ask owner.
use crate::{
    ids::CardId,
    mcp_server::{
        framing::{RpcError, calm_error},
        registry::{
            AppContext, ToolCallIdentity, ToolDescriptor, ToolRegistry,
            role_gated_write_annotations,
        },
    },
    model::CardRole,
    plugin_host::manifest::AgentToolsScope,
    track_plugin_access,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Input {
    Status,
    Request { plugin_id: String, enabled: bool },
}

// Existing requests carry a typed read-only result through rollback; no fabricated event.
enum RequestOutcome {
    Existing(Option<i64>),
    Failed(crate::error::CalmError),
}
impl From<crate::error::CalmError> for RequestOutcome {
    fn from(error: crate::error::CalmError) -> Self {
        Self::Failed(error)
    }
}
impl From<calm_truth::TruthError> for RequestOutcome {
    fn from(error: calm_truth::TruthError) -> Self {
        Self::Failed(error.into())
    }
}

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(ToolDescriptor {
        name: "neige_access_ask".into(),
        description: include_str!("../../../prompts/tools/neige_access_ask.md").trim_end().into(),
        input_schema: json!({"type":"object","additionalProperties":false,"required":["action"],"properties":{
            "action":{"enum":["status","request"]},"plugin_id":{"type":"string"},"enabled":{"type":"boolean"}
        }}),
        annotations: Some(role_gated_write_annotations()),
        roles: &[CardRole::Planner], listed_for: &[CardRole::Planner],
    }, Arc::new(|ctx, identity, args| Box::pin(async move {
        call(ctx, identity, args).await.map(crate::mcp_server::result::ToolResult::structured)
    })));
}

async fn call(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let input: Input =
        serde_json::from_value(args).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let track = identity
        .track_id
        .as_deref()
        .ok_or_else(|| RpcError::forbidden("requires a Track"))?;
    let row = ctx
        .repo
        .track_get(track)
        .await
        .map_err(|e| calm_error(e.into()))?
        .ok_or_else(|| RpcError::invalid_params("Track not found"))?;
    let host = ctx
        .plugin_host
        .get()
        .ok_or_else(|| RpcError::unavailable("plugin host unavailable"))?;
    match input {
        Input::Status => {
            let scope =
                crate::mcp_server::tool_visibility::plugin_scope_for_track(&ctx, Some(track)).await;
            let extras = track_plugin_access::current_grants(ctx.sqlite_db.as_ref(), track, host)
                .await
                .map_err(calm_error)?;
            let running = host.running_plugin_ids().await;
            let mut plugins = Vec::new();
            for plugin in ctx
                .repo
                .plugins_list_all()
                .await
                .map_err(|e| calm_error(e.into()))?
            {
                let manifest = host.registry().get(&plugin.id);
                let authorization = if row.plugin_scope.as_deref() == Some(&plugin.id) {
                    "owner"
                } else if extras.contains(&plugin.id) {
                    "extra"
                } else if row.plugin_scope.is_none()
                    && manifest.as_ref().is_some_and(|m| scope.allows_manifest(m))
                {
                    "standard"
                } else {
                    "not_authorized"
                };
                let callable = running.contains(&plugin.id)
                    && manifest.as_ref().is_some_and(|m| scope.allows_manifest(m));
                plugins.push(json!({"plugin_id":plugin.id,"enabled":plugin.enabled,"running":running.contains(&plugin.id),
                    "name":manifest.as_ref().map(|m| &m.display_name),"authorization":authorization,"callable":callable,
                    "can_request":row.closed_at.is_none() && row.plugin_scope.is_some() && row.plugin_scope.as_deref()!=Some(&plugin.id)
                        && plugin.enabled && manifest.as_ref().is_some_and(|m| m.agent_tools_scope==AgentToolsScope::Enabled)}));
            }
            Ok(json!({"track_id":track,"plugins":plugins}))
        }
        Input::Request { plugin_id, enabled } => {
            let manifest = host
                .registry()
                .get(&plugin_id)
                .ok_or_else(|| RpcError::invalid_params("plugin not in live registry"))?;
            if manifest.agent_tools_scope != AgentToolsScope::Enabled {
                return Err(RpcError::invalid_params(
                    "owner-only plugins cannot receive extra Track authorization",
                ));
            }
            if enabled && !host.running_plugin_ids().await.contains(&plugin_id) {
                return Err(RpcError::unavailable(
                    "plugin is not running; resolve its global state first",
                ));
            }
            let card = CardId::from(identity.card_id.clone());
            let actor = identity.to_actor_id();
            let result = calm_truth::db::write_with_actor_events_typed::<(), RequestOutcome, _>(
                ctx.repo.as_ref(),
                None,
                &ctx.events,
                &ctx.write,
                move |tx| {
                    Box::pin(async move {
                        if !identity.session_is_active(tx, row.id.as_str()).await? {
                            return Err(crate::error::CalmError::Forbidden(
                                "requesting Planner session is no longer active".into(),
                            )
                            .into());
                        }
                        let (pending, event) =
                            track_plugin_access::request_tx(tx, &card, &plugin_id, enabled).await?;
                        let Some((scope, event)) = event else {
                            return Err(RequestOutcome::Existing(pending));
                        };
                        Ok(((), vec![(actor, scope, event)]))
                    })
                },
            )
            .await;
            let pending = match result {
                Ok(((), ids)) => match ids.as_slice() {
                    [id] => Some(*id),
                    _ => {
                        return Err(RpcError::internal(
                            "expected one plugin authorization question",
                        ));
                    }
                },
                Err(RequestOutcome::Existing(pending)) => pending,
                Err(RequestOutcome::Failed(error)) => return Err(calm_error(error)),
            };
            Ok(
                json!({"ask_id":pending, "state":if pending.is_some() {"awaiting_user"} else {"already_set"}}),
            )
        }
    }
}
