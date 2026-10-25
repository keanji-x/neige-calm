//! Durable evidence of the Report snapshot a worker session has seen.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryRef {
    Commit(String),
    Unavailable(HistoryUnavailable),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryUnavailable {
    NotLocated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LastRead {
    pub doc_rev: u64,
    pub whole: bool,
    pub blocks: HashMap<String, ReadBlock>,
    pub sections: HashMap<String, Vec<(String, u32)>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadBlock {
    pub rev: u32,
    pub history_ref: HistoryRef,
}
