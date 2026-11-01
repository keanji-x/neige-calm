use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub track_id: String,
    pub cli_path: PathBuf,
    pub home: PathBuf,
    pub profile: String,
    pub self_open_id: String,
    pub import_state_path: PathBuf,
}
impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if self.track_id.is_empty()
            || self.profile.is_empty()
            || self.self_open_id.is_empty()
            || !self.cli_path.is_absolute()
            || !self.home.is_absolute()
            || !self.import_state_path.is_absolute()
        {
            return Err("feishu-watch requires identity and absolute operator paths".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    Group { chat_id: String },
    IncomingP2p,
    AtMe,
    Thread { thread_id: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceState {
    pub source: Source,
    pub cursor_ms: i64,
    pub fault: Option<super::reader::ReadError>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub at_ms: i64,
    pub sender_id: String,
    pub raw: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Batch {
    pub id: String,
    pub created_at_ms: i64,
    pub messages: BTreeMap<String, Message>,
    pub settlement: Option<Value>,
}
/// A Planner-owned reminder, independent of message arrival. Replacing a deadline re-arms it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deadline {
    pub key: String,
    pub at_ms: i64,
    pub note: String,
    pub fired: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    pub revision: u64,
    /// Complete imported provenance; never interpreted as a new grant of authority.
    pub legacy: Value,
    pub sources: BTreeMap<String, SourceState>,
    pub inbox: BTreeMap<String, Batch>,
    pub seen: BTreeSet<String>,
    pub next_batch: u64,
    pub last_scan_ms: Option<i64>,
    pub interval_ms: i64,
    pub overlap_ms: i64,
    pub due: BTreeMap<String, Deadline>,
    pub reminder_updates: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Wake {
    pub key: String,
    pub text: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct InboxPage {
    pub batch_id: String,
    pub messages: Vec<Message>,
    pub next_after: Option<String>,
    pub settlement: Option<Value>,
}

pub fn timestamp(value: &Value) -> Result<i64, String> {
    if let Some(s) = value.as_str() {
        if let Ok(n) = s.parse::<i64>() {
            return numeric_timestamp(n);
        }
        return chrono::DateTime::parse_from_rfc3339(s)
            .map(|d| d.timestamp_millis())
            .map_err(|_| "invalid timestamp".into());
    }
    value
        .as_i64()
        .ok_or_else(|| "missing timestamp".into())
        .and_then(numeric_timestamp)
}
fn numeric_timestamp(n: i64) -> Result<i64, String> {
    let ms = if n < 1_000_000_000_000 {
        n.checked_mul(1000)
    } else {
        Some(n)
    };
    ms.filter(|n| *n >= 0 && chrono::DateTime::from_timestamp_millis(*n).is_some())
        .ok_or_else(|| "invalid timestamp".into())
}
