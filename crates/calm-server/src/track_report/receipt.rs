//! Final effects of one report operation, independent of any transport budget.
use super::ReportBlock;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockChange {
    pub id: String,
    pub kind: String,
    pub previous_rev: Option<u32>,
    /// Null for a deletion; a live result's final revision, including equal-content writes.
    pub rev: Option<u32>,
    /// Final document position, null for a deletion. Shifted unrelated siblings are omitted.
    pub position: Option<usize>,
    pub moved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommitHistory {
    /// None only when this Track had no history before the write.
    pub before: Option<String>,
    pub after: String,
}

pub(super) fn changes(
    before: &[ReportBlock],
    after: &[ReportBlock],
    written: &[String],
    moved: &[String],
) -> Vec<BlockChange> {
    let old: HashMap<_, _> = before.iter().map(|b| (b.id.as_str(), b)).collect();
    let current: HashSet<_> = after.iter().map(|b| b.id.as_str()).collect();
    let written: HashSet<_> = written.iter().map(String::as_str).collect();
    let moved: HashSet<_> = moved.iter().map(String::as_str).collect();
    let mut result = Vec::new();
    for (position, block) in after.iter().enumerate() {
        let previous = old.get(block.id.as_str()).copied();
        if previous != Some(block)
            || written.contains(block.id.as_str())
            || moved.contains(block.id.as_str())
        {
            result.push(BlockChange {
                id: block.id.clone(),
                kind: block.kind.clone(),
                previous_rev: previous.map(|b| b.rev),
                rev: Some(block.rev),
                position: Some(position),
                moved: moved.contains(block.id.as_str()),
            });
        }
    }
    for block in before.iter().filter(|b| !current.contains(b.id.as_str())) {
        result.push(BlockChange {
            id: block.id.clone(),
            kind: block.kind.clone(),
            previous_rev: Some(block.rev),
            rev: None,
            position: None,
            moved: false,
        });
    }
    result
}
