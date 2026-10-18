//! Bounded discovery over the same picker services REST consumes. Advice grants no admission.
use crate::error::{CalmError, Result};
use crate::routes::idempotency_key::stable_payload_hash;
use crate::state::RouteState;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MAX_BYTES: usize = 65_536;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Template,
    Recipe,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListSources {
    pub kind: SourceKind,
    pub limit: usize,
    pub cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetSource {
    pub kind: SourceKind,
    pub source_id: String,
}

fn bad(reason: &str) -> CalmError {
    CalmError::BadRequest(reason.into())
}

fn bounded(value: Value) -> Result<Value> {
    if serde_json::to_vec(&value)?.len() > MAX_BYTES {
        return Err(bad(
            "catalog result exceeds 65536 bytes; select a smaller source",
        ));
    }
    Ok(value)
}

pub(crate) async fn list(s: &RouteState, request: ListSources) -> Result<Value> {
    if !(1..=50).contains(&request.limit) {
        return Err(bad("catalog limit must be 1..50"));
    }
    let mut entries: Vec<Value> = match request.kind {
        SourceKind::Template => super::track_templates::list(s)
            .await?
            .into_iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<_, _>>()?,
        SourceKind::Recipe => super::track_recipes::list(s)
            .await?
            .into_iter()
            .map(|recipe| {
                // List carries identity/revision; body belongs to the bounded detail read.
                let mut value = serde_json::to_value(recipe)?;
                value.as_object_mut().expect("recipe object").remove("body");
                Ok(value)
            })
            .collect::<std::result::Result<_, serde_json::Error>>()?,
    };
    entries.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let revision = stable_payload_hash(&json!({"kind": request.kind, "entries": entries}))?;
    let start = match request.cursor {
        None => 0,
        Some(cursor) => {
            if cursor.len() > 160 {
                return Err(bad("invalid catalog cursor"));
            }
            let fields: Vec<_> = cursor.split(':').collect();
            if fields.len() != 3 || fields[0] != "v1" {
                return Err(bad("invalid catalog cursor"));
            }
            if fields[1] != revision {
                return Err(CalmError::Conflict(
                    "catalog changed; restart discovery without cursor".into(),
                ));
            }
            let offset: usize = fields[2]
                .parse()
                .map_err(|_| bad("invalid catalog cursor"))?;
            if offset == 0 || offset >= entries.len() {
                return Err(bad("invalid catalog cursor"));
            }
            offset
        }
    };
    let mut end = (start + request.limit).min(entries.len());
    loop {
        let next = (end < entries.len()).then(|| format!("v1:{revision}:{end}"));
        let page = json!({"kind": request.kind, "items": entries[start..end], "revision": revision, "next_cursor": next});
        if serde_json::to_vec(&page)?.len() <= MAX_BYTES {
            return Ok(page);
        }
        if end <= start + 1 {
            return Err(bad("catalog entry exceeds result budget"));
        }
        end -= 1;
    }
}

pub(crate) async fn get(s: &RouteState, request: GetSource) -> Result<Value> {
    if request.source_id.trim().is_empty() || request.source_id.len() > 1024 {
        return Err(bad(
            "catalog source id must be nonempty and at most 1024 bytes",
        ));
    }
    let source = match request.kind {
        SourceKind::Template => {
            let entries = super::track_templates::list(s).await?;
            let summary = entries
                .into_iter()
                .find(|entry| entry.id == request.source_id)
                .ok_or_else(|| CalmError::NotFound(format!("template {}", request.source_id)))?;
            let mut value = serde_json::to_value(summary)?;
            let detail =
                serde_json::to_value(super::track_templates::detail(s, &request.source_id)?)?;
            value
                .as_object_mut()
                .expect("template object")
                .extend(detail.as_object().expect("detail object").clone());
            value["plugin_guides"] =
                serde_json::to_value(super::track_templates::guides(s, &request.source_id)?)?;
            value
        }
        SourceKind::Recipe => {
            serde_json::to_value(super::track_recipes::detail(s, &request.source_id).await?)?
        }
    };
    bounded(json!({"kind": request.kind, "source": source}))
}
