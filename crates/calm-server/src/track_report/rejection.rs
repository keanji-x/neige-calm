//! Report-owned refusals. Evidence is frozen where the guard fails, inside persist's transaction.
use super::{ReportBlock, ReportDoc, block_op_internal};
use crate::{error::CalmError, report_read_ledger::LastRead};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    NeverRead,
    ChangedSince,
    WholeRequired,
    SupersededSession,
}

#[derive(Debug, Clone)]
pub enum Scope {
    Document,
    Whole,
    Block(String),
    Section(String),
}

#[derive(Debug, Serialize)]
pub struct Target {
    report_card_id: String,
    block_ids: Vec<String>,
    sections: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct NextStep {
    action: &'static str,
    blocks: Vec<String>,
    sections: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct Evidence {
    pub reason_code: Reason,
    target: Target,
    next_step: NextStep,
    changed: Vec<crate::report_read_ledger::ChangedRevision>,
    doc_rev: u64,
}

/// Guard-local evidence before persist attaches the authenticated Report identity.
#[derive(Debug)]
pub struct Witness {
    block_ids: Vec<String>,
    sections: Vec<String>,
    next_step: NextStep,
    changed: Vec<crate::report_read_ledger::ChangedRevision>,
    doc_rev: u64,
}

/// Kept outside CalmError and the event layer: only the Report MCP adapter consumes advice.
#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error(transparent)]
    Other(#[from] CalmError),
    #[error("{source}")]
    Anchor {
        #[source]
        source: CalmError,
        reason: Reason,
        scope: Scope,
        evidence: Option<Box<Witness>>,
    },
    #[error("{source}")]
    Rejected {
        source: CalmError,
        evidence: Box<Evidence>,
    },
    #[error("{0}")]
    Superseded(CalmError),
}

impl From<calm_truth::TruthError> for ReportError {
    fn from(e: calm_truth::TruthError) -> Self {
        Self::Other(e.into())
    }
}
impl From<sqlx::Error> for ReportError {
    fn from(e: sqlx::Error) -> Self {
        Self::Other(e.into())
    }
}
impl From<ReportError> for CalmError {
    fn from(e: ReportError) -> Self {
        match e {
            ReportError::Other(e)
            | ReportError::Superseded(e)
            | ReportError::Anchor { source: e, .. }
            | ReportError::Rejected { source: e, .. } => e,
        }
    }
}

impl ReportError {
    pub(super) fn anchor(source: CalmError, reason: Reason, scope: Scope) -> Self {
        Self::Anchor {
            source,
            reason,
            scope,
            evidence: None,
        }
    }

    pub(super) fn at_step(self, index: usize) -> Self {
        fn prefix(e: CalmError, index: usize) -> CalmError {
            match e {
                CalmError::Conflict(m) => CalmError::Conflict(format!("ops[{index}]: {m}")),
                CalmError::BadRequest(m) => CalmError::BadRequest(format!("ops[{index}]: {m}")),
                other => other,
            }
        }
        match self {
            Self::Other(e) => Self::Other(prefix(e, index)),
            Self::Anchor {
                source,
                reason,
                scope,
                evidence,
            } => Self::Anchor {
                source: prefix(source, index),
                reason,
                scope,
                evidence,
            },
            other => other,
        }
    }

    /// No query or read certification; this consumes the same working doc and temporary batch knowledge as the guard.
    pub(super) fn freeze(self, doc: &ReportDoc, known: Option<&LastRead>) -> Self {
        let Self::Anchor {
            source,
            mut reason,
            scope,
            evidence: None,
        } = self
        else {
            return self;
        };
        if let Scope::Block(id) = &scope
            && known.and_then(|k| k.blocks.get(id)).is_none()
        {
            reason = Reason::NeverRead;
        }
        let build = || -> Result<Witness, CalmError> {
            let blocks = doc.blocks_snapshot().map_err(block_op_internal)?;
            let doc_rev = doc.doc_rev().map_err(block_op_internal)?;
            let mut prior = BTreeMap::new();
            let mut current: Vec<&ReportBlock> = Vec::new();
            let mut ids = Vec::new();
            let mut sections = Vec::new();
            let mut next = NextStep {
                action: "read",
                blocks: vec![],
                sections: vec![],
                mode: None,
            };
            match &scope {
                Scope::Block(id) => {
                    ids.push(id.clone());
                    if let Some(old) = known.and_then(|k| k.blocks.get(id)) {
                        prior.insert(id.clone(), old.rev);
                    }
                    current.extend(blocks.iter().filter(|b| &b.id == id));
                    if current.is_empty() {
                        next.action = "index/revise_target";
                    } else {
                        next.blocks = ids.clone();
                    }
                }
                Scope::Section(name) => {
                    sections.push(name.clone());
                    let seen = known.and_then(|k| k.sections.get(name));
                    let mut related: std::collections::BTreeSet<String> = seen
                        .into_iter()
                        .flatten()
                        .map(|(id, _)| id.clone())
                        .collect();
                    prior.extend(related.iter().filter_map(|id| {
                        known
                            .and_then(|k| k.blocks.get(id))
                            .map(|block| (id.clone(), block.rev))
                    }));
                    match crate::report_sections::find_section(&blocks, name) {
                        Ok(Some(section)) => {
                            let members = section.member_blocks(&blocks);
                            for block in &members {
                                related.insert(block.id.clone());
                                if let Some(old) = known.and_then(|k| k.blocks.get(&block.id)) {
                                    prior.entry(block.id.clone()).or_insert(old.rev);
                                }
                            }
                            current = blocks.iter().filter(|b| related.contains(&b.id)).collect();
                            next.sections = sections.clone();
                        }
                        Ok(None) if seen.is_some() => {
                            next.mode = Some("full");
                            current = blocks.iter().filter(|b| related.contains(&b.id)).collect();
                        }
                        _ => {
                            next.action = "index/revise_target";
                            current = blocks.iter().filter(|b| related.contains(&b.id)).collect();
                        }
                    }
                }
                Scope::Document | Scope::Whole => {
                    // Whole/text reads do not expose or certify unread task revisions.
                    current.extend(
                        blocks
                            .iter()
                            .filter(|b| calm_types::report_blocks::in_sections(&b.kind)),
                    );
                    prior.extend(
                        known
                            .into_iter()
                            .flat_map(|k| k.blocks.iter())
                            .filter(|(id, _)| {
                                blocks
                                    .iter()
                                    .find(|b| &b.id == *id)
                                    .is_none_or(|b| calm_types::report_blocks::in_sections(&b.kind))
                            })
                            .map(|(id, b)| (id.clone(), b.rev)),
                    );
                    if matches!(scope, Scope::Whole) {
                        next.mode = Some("full");
                    } else if let Some(block) = current.first() {
                        next.blocks.push(block.id.clone());
                    } else {
                        next.mode = Some("full");
                    }
                }
            }
            let changed = crate::report_read_ledger::changed_revisions(prior, &current);
            Ok(Witness {
                block_ids: ids,
                sections,
                next_step: next,
                changed,
                doc_rev,
            })
        };
        match build() {
            Ok(evidence) => Self::Anchor {
                source,
                reason,
                scope,
                evidence: Some(Box::new(evidence)),
            },
            Err(e) => Self::Other(e),
        }
    }

    pub(super) fn for_report(self, report: &str, op: &super::ReportDocOp, doc: &ReportDoc) -> Self {
        if let Self::Anchor {
            source,
            reason,
            scope,
            evidence: Some(mut e),
        } = self
        {
            if matches!(scope, Scope::Document) {
                if let super::ReportDocOp::AgentBatch { ops, .. } = op {
                    for op in ops {
                        use super::BlockIntent;
                        match op {
                            BlockIntent::Upsert { id: Some(id), .. }
                            | BlockIntent::Move { id, .. }
                            | BlockIntent::Delete { id } => {
                                if !e.block_ids.contains(id) {
                                    e.block_ids.push(id.clone());
                                }
                            }
                            BlockIntent::ReplaceSection { section, .. }
                            | BlockIntent::DeleteSection { section }
                                if !e.sections.contains(section) =>
                            {
                                e.sections.push(section.clone());
                            }
                            _ => (),
                        }
                    }
                }
                for id in &e.block_ids {
                    match doc.block_rev(id) {
                        Ok(None) => {
                            e.next_step.action = "index/revise_target";
                            e.next_step.blocks.clear();
                            e.next_step.sections.clear();
                            e.next_step.mode = None;
                        }
                        Ok(Some(_)) => (),
                        Err(error) => return Self::Other(block_op_internal(error)),
                    }
                }
            }
            Self::Rejected {
                source,
                evidence: Box::new(Evidence {
                    reason_code: reason,
                    target: Target {
                        report_card_id: report.to_owned(),
                        block_ids: e.block_ids,
                        sections: e.sections,
                    },
                    next_step: e.next_step,
                    changed: e.changed,
                    doc_rev: e.doc_rev,
                }),
            }
        } else {
            self
        }
    }
}
