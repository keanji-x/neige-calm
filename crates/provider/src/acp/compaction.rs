//! Standard ACP compaction updates. Native IDs are opaque session-local identities.
//! No update establishes manual-operation ownership or execution completion.
use super::Error;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    InProgress,
    Completed,
    Cancelled,
    Failed,
}

/// ACP optional fields are patches: absence preserves, null clears, a value replaces.
#[derive(Clone, Debug, PartialEq)]
pub enum Patch<T> {
    Unchanged,
    Clear,
    Set(T),
}

#[derive(Clone, Debug, PartialEq)]
pub enum CompactionUpdate {
    Lifecycle {
        compaction_id: String,
        status: Status,
        /// ACP content blocks are retained without flattening rich content into text.
        summary: Patch<Vec<Value>>,
        error: Patch<String>,
    },
    SummaryChunk {
        compaction_id: String,
        content: Value,
    },
}
impl CompactionUpdate {
    /// Decode the update object, not the enclosing session notification.
    /// Unknown update kinds (including legacy vendor markers) remain ignored.
    pub fn decode(update: &Value) -> Result<Option<Self>, Error> {
        let kind = update["sessionUpdate"].as_str();
        if !matches!(kind, Some("compaction_update" | "compaction_summary_chunk")) {
            return Ok(None);
        }
        let id = update["compactionId"]
            .as_str()
            .ok_or(Error::Protocol("missing compaction identity"))?
            .to_owned();
        if kind == Some("compaction_summary_chunk") {
            let content = update["content"].clone();
            validate_content(&content)?;
            return Ok(Some(Self::SummaryChunk {
                compaction_id: id,
                content,
            }));
        }
        let status: Status = serde_json::from_value(update["status"].clone())
            .map_err(|_| Error::Protocol("invalid compaction status"))?;
        let summary: Patch<Vec<Value>> = patch(update, "summary")?;
        let error: Patch<String> = patch(update, "error")?;
        if let Patch::Set(blocks) = &summary {
            for block in blocks {
                validate_content(block)?;
            }
            if !blocks.is_empty() && status != Status::Completed {
                return Err(Error::Protocol("compaction summary before completion"));
            }
        }
        if matches!(error, Patch::Set(_)) && status != Status::Failed {
            return Err(Error::Protocol("compaction error without failure"));
        }
        Ok(Some(Self::Lifecycle {
            compaction_id: id,
            status,
            summary,
            error,
        }))
    }
}
fn patch<T: serde::de::DeserializeOwned>(value: &Value, key: &str) -> Result<Patch<T>, Error> {
    match value.get(key) {
        None => Ok(Patch::Unchanged),
        Some(Value::Null) => Ok(Patch::Clear),
        Some(value) => serde_json::from_value(value.clone())
            .map(Patch::Set)
            .map_err(|_| Error::Protocol("invalid compaction patch")),
    }
}
fn validate_content(content: &Value) -> Result<(), Error> {
    // Rich ACP blocks stay opaque, as in the existing tool-content translation.
    match content["type"].as_str() {
        Some("text") if !content["text"].is_string() => {
            Err(Error::Protocol("missing compaction text"))
        }
        Some(_) => Ok(()),
        None => Err(Error::Protocol("missing compaction content type")),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Compaction {
    pub status: Status,
    pub summary: Vec<Value>,
    pub error: Option<String>,
}
#[derive(Default)]
pub(super) struct Compactions {
    pub entries: BTreeMap<String, Compaction>,
    received_bytes: usize,
}
impl Compactions {
    pub fn apply(&mut self, update: CompactionUpdate, wire_bytes: usize) -> Result<(), Error> {
        // Bound retained state just as the translator bounds ordinary text/tools.
        // Count all received compaction bytes conservatively, including replacements.
        let received_bytes = self.received_bytes.saturating_add(wire_bytes);
        if received_bytes > 16 * 1024 * 1024 {
            return Err(Error::Protocol("turn compaction data exceeds byte limit"));
        }
        match update {
            CompactionUpdate::Lifecycle {
                compaction_id,
                status,
                summary,
                error,
            } => {
                if self.entries.len() >= 4096 && !self.entries.contains_key(&compaction_id) {
                    return Err(Error::Protocol("turn compaction count exceeds limit"));
                }
                let entry = self
                    .entries
                    .entry(compaction_id)
                    .or_insert_with(|| Compaction {
                        status,
                        summary: Vec::new(),
                        error: None,
                    });
                entry.status = status;
                match summary {
                    Patch::Unchanged => {}
                    Patch::Clear => entry.summary.clear(),
                    Patch::Set(summary) => entry.summary = summary,
                }
                match error {
                    Patch::Unchanged => {}
                    Patch::Clear => entry.error = None,
                    Patch::Set(error) => entry.error = Some(error),
                }
            }
            CompactionUpdate::SummaryChunk {
                compaction_id,
                content,
            } => {
                let entry = self
                    .entries
                    .get_mut(&compaction_id)
                    .filter(|entry| entry.status == Status::InProgress)
                    .ok_or(Error::Protocol(
                        "compaction chunk without matching in-progress identity",
                    ))?;
                entry.summary.push(content);
            }
        }
        self.received_bytes = received_bytes;
        Ok(())
    }
}
