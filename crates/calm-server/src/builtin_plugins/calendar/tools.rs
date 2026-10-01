use super::store::{self, Access};
use crate::mcp_server::{
    framing::RpcError,
    registry::{
        ToolCallIdentity, ToolDescriptor, ToolRegistry, read_only_annotations, require_role_any,
        role_gated_write_annotations,
    },
    result::ToolResult,
};
use crate::{event::EventScope, model::CardRole};
use serde_json::{Value, json};
use std::sync::Arc;
const ROLES: &[CardRole] = &[CardRole::Planner, CardRole::Assistant];
fn access(identity: &ToolCallIdentity) -> Result<Access, RpcError> {
    require_role_any(identity, ROLES)?;
    let track = identity
        .track_id
        .clone()
        .ok_or_else(|| RpcError::invalid_params("calendar requires a Track"))?;
    Ok(Access {
        track: Some(track.clone()),
        actor: identity.to_actor_id(),
        scope: EventScope::Card {
            card: identity.card_id.clone().into(),
            track: track.into(),
            area: identity.area_id.clone().into(),
        },
        creator: format!("card:{}", identity.card_id),
    })
}
fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, RpcError> {
    serde_json::from_value(value).map_err(|e| RpcError::invalid_params(e.to_string()))
}
pub fn register(registry: &mut ToolRegistry) {
    let schedule = json!({"oneOf":[
        {"type":"object","additionalProperties":false,"required":["kind","date"],"properties":{"kind":{"const":"all_day"},"date":{"type":"string"}}},
        {"type":"object","additionalProperties":false,"required":["kind","start","end","timezone"],"properties":{"kind":{"const":"timed"},"start":{"type":"string"},"end":{"type":"string"},"timezone":{"type":"string"}}}
    ]});
    let task = json!({"type":"object","additionalProperties":false,"required":["title","description","schedule"],"properties":{"title":{"type":"string"},"description":{"type":"string"},"schedule":schedule}});
    for action in ["list", "create", "update"] {
        let schema = match action {
            "list" => {
                json!({"type":"object","additionalProperties":false,"required":["from","until","timezone"],"properties":{"from":{"type":"string"},"until":{"type":"string"},"timezone":{"type":"string"}}})
            }
            "create" => {
                json!({"type":"object","additionalProperties":false,"required":["idempotency_key","task"],"properties":{"idempotency_key":{"type":"string"},"task":task}})
            }
            _ => {
                json!({"type":"object","additionalProperties":false,"required":["id","expected_version","task","cancelled"],"properties":{"id":{"type":"string"},"expected_version":{"type":"integer"},"task":task,"cancelled":{"type":"boolean"}}})
            }
        };
        registry.register(
            ToolDescriptor {
                name: format!("calm.calendar.{action}"),
                description: format!("{action}: {}", include_str!("tool-help.md").trim()),
                input_schema: schema,
                annotations: Some(if action == "list" {
                    read_only_annotations()
                } else {
                    role_gated_write_annotations()
                }),
                visible_to_roles: ROLES,
            },
            Arc::new(move |ctx, identity, mut args| {
                Box::pin(async move {
                    let access = access(&identity)?;
                    let result = match action {
                        "list" => store::list(&ctx, &access, parse(args)?)
                            .await
                            .map(|v| json!(v)),
                        "create" => store::create(&ctx, access, parse(args)?)
                            .await
                            .map(|v| json!(v)),
                        _ => {
                            let id = args
                                .as_object_mut()
                                .and_then(|obj| obj.remove("id"))
                                .and_then(|v| v.as_str().map(str::to_owned))
                                .ok_or_else(|| RpcError::invalid_params("id required"))?;
                            store::update(&ctx, access, id, parse(args)?)
                                .await
                                .map(|v| json!(v))
                        }
                    };
                    result
                        .map(ToolResult::structured)
                        .map_err(|e| RpcError::custom(-32000, e.to_string()))
                })
            }),
        );
    }
}
