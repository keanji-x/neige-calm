use super::{
    model::*,
    ports::Storage,
    store::{self, Access, Change},
};
use crate::{builtin::tools::NativeToolSpec, mcp::RpcError, ports::ErrorFactory};
use calm_types::model::CardRole;
use serde::Deserialize;
use serde_json::{Value, json};
const ROLES: &[CardRole] = &[CardRole::Planner, CardRole::Assistant];
/// The registry leads every refusal with the served name (`plugin_calendar_ls: …`).
fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, RpcError> {
    serde_json::from_value(value).map_err(|e| RpcError::invalid_params(e.to_string()))
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
/// The tools name an entry's id `entry_id` and give its times as RFC 3339 strings (§4); the stored
/// entry and the REST payload keep `id` and unix ms.
fn entry_output(mut entry: Value, format: fn(&mut Value, &[&str])) -> Value {
    if let Some(obj) = entry.as_object_mut()
        && let Some(id) = obj.remove("id")
    {
        obj.insert("entry_id".into(), id);
    }
    format(&mut entry, &["created_at", "updated_at"]);
    entry
}
pub fn descriptors() -> Vec<NativeToolSpec> {
    let mut descriptors = Vec::new();
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
        descriptors.push(NativeToolSpec {
            // The local name; the component serves it as `plugin_calendar_<action>` (#2227).
            name: action.into(),
            description: match action {
                "ls" => include_str!("prompts/plugin_calendar_ls.md"),
                "add" => include_str!("prompts/plugin_calendar_add.md"),
                "set" => include_str!("prompts/plugin_calendar_set.md"),
                _ => include_str!("prompts/plugin_calendar_rm.md"),
            }
            .trim_end()
            .to_string(),
            input_schema: schema,
            annotations: Some(if action == "ls" {
                calm_types::plugin::read_only_annotations()
            } else {
                calm_types::plugin::role_gated_write_annotations()
            }),
            roles: ROLES,
            listed_for: ROLES,
        });
    }
    descriptors
}
pub async fn call<S: Storage>(
    storage: &S,
    access: Access,
    action: &str,
    args: Value,
    format: fn(&mut Value, &[&str]),
) -> Result<Value, RpcError> {
    let result = match action {
        "ls" => {
            let Ls { from, to, timezone } = parse(args)?;
            let window = Window {
                from,
                until: to,
                timezone,
            };
            // Rows under `entries` (§5): a result is never a bare array.
            store::list(storage, &access, window).await.map(|listed| {
                let entries: Vec<Value> = listed
                    .into_iter()
                    .map(|v| entry_output(json!(v), format))
                    .collect();
                json!({ "entries": entries })
            })
        }
        "add" => store::create(storage, access, parse(args)?)
            .await
            .map(|v| entry_output(json!(v), format)),
        "set" => {
            let Set {
                entry_id,
                expected_version,
                task,
            } = parse(args)?;
            store::update(
                storage,
                access,
                entry_id,
                expected_version,
                Change::Edit(task),
            )
            .await
            .map(|v| entry_output(json!(v), format))
        }
        _ => {
            let Rm {
                entry_id,
                expected_version,
            } = parse(args)?;
            store::update(storage, access, entry_id, expected_version, Change::Remove)
                .await
                .map(|v| entry_output(json!(v), format))
        }
    };
    result.map_err(|error| error.to_rpc())
}
