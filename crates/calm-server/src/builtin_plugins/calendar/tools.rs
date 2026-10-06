use super::model::{Draft, Window};
use super::store::{self, Access, Change};
use crate::mcp_server::{
    framing::RpcError,
    registry::{
        ToolCallIdentity, ToolDescriptor, ToolRegistry, read_only_annotations, require_role_any,
        role_gated_write_annotations,
    },
    result::ToolResult,
};
use crate::{event::EventScope, model::CardRole};
use serde::Deserialize;
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
fn parse<T: serde::de::DeserializeOwned>(action: &str, value: Value) -> Result<T, RpcError> {
    serde_json::from_value(value)
        .map_err(|e| RpcError::invalid_params(format!("neige_calendar_{action}: {e}")))
}
/// The tools' half-open `[from, to)` window; the REST route and the store keep `until`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ls {
    from: String,
    to: String,
    timezone: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Set {
    entry_id: String,
    expected_version: i64,
    task: Draft,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rm {
    entry_id: String,
    expected_version: i64,
}
/// The tools name an entry's id `entry_id` (§4); the stored entry and the REST payload keep `id`.
fn entry_output(mut entry: Value) -> Value {
    if let Some(obj) = entry.as_object_mut()
        && let Some(id) = obj.remove("id")
    {
        obj.insert("entry_id".into(), id);
    }
    entry
}
pub fn register(registry: &mut ToolRegistry) {
    let schedule = json!({"oneOf":[
        {"type":"object","additionalProperties":false,"required":["kind","date"],"properties":{"kind":{"const":"all_day"},"date":{"type":"string"}}},
        {"type":"object","additionalProperties":false,"required":["kind","start","end","timezone"],"properties":{"kind":{"const":"timed"},"start":{"type":"string"},"end":{"type":"string"},"timezone":{"type":"string"}}},
        {"type":"object","additionalProperties":false,"required":["kind","weekdays","start","end","timezone","from"],"properties":{"kind":{"const":"weekly"},"weekdays":{"type":"array","items":{"enum":["mon","tue","wed","thu","fri","sat","sun"]}},"start":{"type":"string"},"end":{"type":"string"},"timezone":{"type":"string"},"from":{"type":"string"},"until":{"type":"string"}}}
    ]});
    let task = json!({"type":"object","additionalProperties":false,"required":["title","description","schedule"],"properties":{"title":{"type":"string"},"description":{"type":"string"},"schedule":schedule}});
    for action in ["ls", "add", "set", "rm"] {
        let schema = match action {
            "ls" => {
                json!({"type":"object","additionalProperties":false,"required":["from","to","timezone"],"properties":{"from":{"type":"string"},"to":{"type":"string"},"timezone":{"type":"string"}}})
            }
            "add" => {
                json!({"type":"object","additionalProperties":false,"required":["idempotency_key","task"],"properties":{"idempotency_key":{"type":"string"},"task":task}})
            }
            "set" => {
                json!({"type":"object","additionalProperties":false,"required":["entry_id","expected_version","task"],"properties":{"entry_id":{"type":"string"},"expected_version":{"type":"integer"},"task":task}})
            }
            _ => {
                json!({"type":"object","additionalProperties":false,"required":["entry_id","expected_version"],"properties":{"entry_id":{"type":"string"},"expected_version":{"type":"integer"}}})
            }
        };
        registry.register(
            ToolDescriptor {
                name: format!("neige_calendar_{action}"),
                description: match action {
                    "ls" => include_str!("../../../prompts/tools/neige_calendar_ls.md"),
                    "add" => include_str!("../../../prompts/tools/neige_calendar_add.md"),
                    "set" => include_str!("../../../prompts/tools/neige_calendar_set.md"),
                    _ => include_str!("../../../prompts/tools/neige_calendar_rm.md"),
                }
                .trim_end()
                .to_string(),
                input_schema: schema,
                annotations: Some(if action == "ls" {
                    read_only_annotations()
                } else {
                    role_gated_write_annotations()
                }),
                visible_to_roles: ROLES,
            },
            Arc::new(move |ctx, identity, args| {
                Box::pin(async move {
                    let access = access(&identity)?;
                    let result = match action {
                        "ls" => {
                            let Ls { from, to, timezone } = parse(action, args)?;
                            let window = Window {
                                from,
                                until: to,
                                timezone,
                            };
                            // Rows under `entries` (§5): a result is never a bare array.
                            store::list(&ctx, &access, window).await.map(|listed| {
                                let entries: Vec<Value> =
                                    listed.into_iter().map(|v| entry_output(json!(v))).collect();
                                json!({ "entries": entries })
                            })
                        }
                        "add" => store::create(&ctx, access, parse(action, args)?)
                            .await
                            .map(|v| entry_output(json!(v))),
                        "set" => {
                            let Set {
                                entry_id,
                                expected_version,
                                task,
                            } = parse(action, args)?;
                            store::update(
                                &ctx,
                                access,
                                entry_id,
                                expected_version,
                                Change::Edit(task),
                            )
                            .await
                            .map(|v| entry_output(json!(v)))
                        }
                        _ => {
                            let Rm {
                                entry_id,
                                expected_version,
                            } = parse(action, args)?;
                            store::update(&ctx, access, entry_id, expected_version, Change::Remove)
                                .await
                                .map(|v| entry_output(json!(v)))
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
