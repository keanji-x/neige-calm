//! Read-only native transcript projection; source selection belongs to the card's declared profile.
use super::*;
use crate::model::HarnessItem;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum EntryKey {
    Item(String),
    Row(i64),
}

pub(super) async fn conversation(
    repo: &dyn RouteRepo,
    card: &CardId,
) -> Result<String, TrackFsError> {
    let mut after = 0;
    let mut order = Vec::new();
    let mut latest = BTreeMap::<EntryKey, HarnessItem>::new();
    loop {
        let rows = repo
            .harness_item_list_transcript_by_card(card.as_str(), after, 500, false)
            .await
            .map_err(|error| TrackFsError::Internal(format!("native transcript read: {error}")))?;
        let count = rows.len();
        for row in rows {
            after = row.id;
            let key = row
                .item_uuid
                .as_ref()
                .filter(|id| !id.is_empty())
                .map(|id| EntryKey::Item(id.clone()))
                .unwrap_or(EntryKey::Row(row.id));
            if !latest.contains_key(&key) {
                order.push(key.clone());
            }
            latest.insert(key, row);
        }
        if count == 0 {
            break;
        }
    }
    let mut out = format!(
        "> READ-ONLY PROJECTION: persisted native transcript.\n\n# Conversation — card {card}\n\n"
    );
    for key in order {
        let row = &latest[&key];
        let params: Value = serde_json::from_str(&row.params)
            .map_err(|error| TrackFsError::Internal(format!("native transcript JSON: {error}")))?;
        if row.method == "turn/completed" {
            out.push_str("## Turn outcome\n\n");
            json_block(&mut out, &params)?;
            continue;
        }
        let item = params
            .get("item")
            .filter(|item| item.is_object())
            .unwrap_or(&params);
        let item_type = item.get("type").and_then(Value::as_str);
        match item_type {
            Some("agentMessage") if item["text"].is_string() => {
                out.push_str("## Assistant\n\n");
                out.push_str(item["text"].as_str().expect("checked string"));
                out.push_str("\n\n");
            }
            _ if calm_types::harness::is_native_user_message_type(item_type) => {
                out.push_str("## Input\n\n");
                let texts = item["content"].as_array().map(|blocks| {
                    blocks
                        .iter()
                        .map(|block| block["text"].as_str())
                        .collect::<Option<Vec<_>>>()
                });
                match texts {
                    Some(Some(texts)) => {
                        out.push_str(&texts.join("\n"));
                        out.push_str("\n\n");
                    }
                    _ => json_block(&mut out, item)?,
                }
            }
            _ => {
                out.push_str("## Native item\n\n");
                json_block(&mut out, item)?;
            }
        }
    }
    Ok(out)
}

fn json_block(out: &mut String, value: &Value) -> Result<(), TrackFsError> {
    let body = serde_json::to_string_pretty(value)
        .map_err(|error| TrackFsError::Internal(format!("native transcript render: {error}")))?;
    out.push_str("```json\n");
    out.push_str(&body);
    out.push_str("\n```\n\n");
    Ok(())
}

impl TrackFsView<'_> {
    pub(super) async fn uses_native_transcript(&self, card: &Card) -> Result<bool, TrackFsError> {
        if card.payload.get("harness_profile").and_then(Value::as_str) != Some("task_worker") {
            return Ok(false);
        }
        let role = self
            .repo
            .card_role_get(card.id.as_str())
            .await
            .map_err(|error| TrackFsError::Internal(format!("native transcript role: {error}")))?
            .ok_or_else(|| path_not_available(card.id.as_str()))?;
        Ok(
            crate::planner_binding::profile_from_shape(&card.kind, role, &card.payload)
                == Some(calm_types::harness::HarnessProfile::TaskWorker),
        )
    }
}
