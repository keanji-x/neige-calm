//! Agent intent carries no revisions. Bind each step against transactional knowledge.
use super::rejection::{Reason, ReportError, Scope};
use super::{BatchBlockOp, DocAnchor, ReportDocOp, SectionRead};
use crate::error::CalmError;
use crate::report_read_ledger::LastRead;

#[derive(Debug, Clone)]
pub enum BlockIntent {
    Upsert {
        id: Option<String>,
        kind: String,
        content: String,
        position: Option<usize>,
    },
    Move {
        id: String,
        to_index: usize,
    },
    Delete {
        id: String,
    },
    ReplaceSection {
        section: String,
        markdown: String,
    },
    DeleteSection {
        section: String,
    },
}

impl BlockIntent {
    pub(super) fn bind(&self, read: Option<&LastRead>) -> Result<BatchBlockOp, ReportError> {
        let block = |id: &str| {
            read.and_then(|r| r.blocks.get(id).map(|block| block.rev)).ok_or_else(||
            ReportError::anchor(CalmError::BadRequest(format!("block `{id}` has not been read by this session — read it first (neige_report_read {{ blocks: [\"{id}\"] }})")), Reason::NeverRead, Scope::Block(id.into())))
        };
        let section_read = |s: &str| {
            read.and_then(|r| r.sections.get(s))
                .map_or(SectionRead::Unseen, |v| SectionRead::Seen(v.clone()))
        };
        Ok(match self {
            Self::Upsert {
                id,
                kind,
                content,
                position,
            } => BatchBlockOp::Upsert {
                id: id.clone(),
                kind: kind.clone(),
                content: content.clone(),
                position: *position,
                if_rev: id.as_deref().map(block).transpose()?,
            },
            Self::Move { id, to_index } => BatchBlockOp::Move {
                id: id.clone(),
                to_index: *to_index,
            },
            Self::Delete { id } => BatchBlockOp::Delete {
                id: id.clone(),
                if_rev: block(id)?,
            },
            Self::ReplaceSection { section, markdown } => BatchBlockOp::ReplaceSection {
                section: section.clone(),
                markdown: markdown.clone(),
                read: section_read(section),
            },
            Self::DeleteSection { section } => BatchBlockOp::DeleteSection {
                section: section.clone(),
                read: section_read(section),
            },
        })
    }
}

pub(super) fn header(
    op: &ReportDocOp,
    read: Option<&LastRead>,
) -> Result<ReportDocOp, ReportError> {
    Ok(match op {
        ReportDocOp::AgentBatch { summary, .. } => ReportDocOp::Batch {
            doc_anchor: read.map_or(DocAnchor::Unread, |r| DocAnchor::LastRead(r.doc_rev)),
            summary: summary.clone(),
            ops: vec![],
        },
        ReportDocOp::AgentWrite { summary, body } => {
            let read = read.filter(|r| r.whole).ok_or_else(|| ReportError::anchor(CalmError::BadRequest(
                "this session has not read the whole report — read it with a full neige_report_read, then retry".into()), Reason::WholeRequired, Scope::Whole))?;
            ReportDocOp::WriteMarkdown {
                summary: summary.clone(),
                body: body.clone(),
                if_doc_rev: read.doc_rev,
            }
        }
        other => other.clone(),
    })
}
