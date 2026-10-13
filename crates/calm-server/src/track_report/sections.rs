//! The section ops of a [`super::ReportDocOp::Batch`] (#1877), inside the persist tx: the anchor is
//! the section's ordered `(id, rev)` list as this session last read it, compared to the doc here.
//! A section op reaches the section's members only, never a task block in its span (#2598).

use super::{DocAnchor, SectionRead, block_op_internal, check_doc_anchor};
use crate::error::CalmError;
use crate::report_sections::{
    Section, block_h1_title, find_section, h1_title, section_error_message,
};
use crate::track_report_doc::ReportDoc;
use crate::track_report_guard::validate_body_fences;
use calm_types::report_blocks::{flat_text, in_sections, parse_fence, strip_markers_and_split};
use calm_types::report_contract::parse_line;
use calm_types::track_report::ReportBlock;

fn snapshot(doc: &ReportDoc) -> Result<Vec<ReportBlock>, CalmError> {
    doc.blocks_snapshot()
        .map_err(|e| CalmError::Internal(format!("track_report: section snapshot: {e}")))
}

/// Section `name`, its members checked against this session's read of it; a section the session read
/// that is gone now is a conflict for every section op, a section it never read that is gone is `None`.
fn locate_read<'a>(
    blocks: &'a [ReportBlock],
    name: &str,
    read: &SectionRead,
) -> Result<Option<Section<'a>>, CalmError> {
    let found = find_section(blocks, name)
        .map_err(|error| CalmError::BadRequest(section_error_message(blocks, &error)))?;
    match (found, read) {
        (Some(section), _) => {
            check_section_read(name, &section.member_blocks(blocks), read).map(|()| Some(section))
        }
        (None, SectionRead::Seen(_)) => Err(CalmError::Conflict(format!(
            "section `{name}` was removed since this session read it — re-read the report and retry"
        ))),
        (None, SectionRead::Unseen) => Ok(None),
    }
}

/// The implicit check: the section as this session last read it must be the section now.
fn check_section_read(
    name: &str,
    current: &[&ReportBlock],
    read: &SectionRead,
) -> Result<(), CalmError> {
    let SectionRead::Seen(seen) = read else {
        return Err(CalmError::BadRequest(format!(
            "section `{name}` has not been read by this session — read it first \
             (neige_report_read {{ sections: [\"{name}\"] }}), then retry"
        )));
    };
    let now: Vec<(String, u32)> = current
        .iter()
        .map(|block| (block.id.clone(), block.rev))
        .collect();
    if *seen != now {
        return Err(CalmError::Conflict(format!(
            "section `{name}` changed since this session read it — re-read it \
             (sections: [\"{name}\"]), merge, and retry"
        )));
    }
    Ok(())
}

/// Replace section `name` with `markdown` under `write_markdown` rules, matching old and new blocks
/// inside the section only; an absent section the contract declares is created at its declared
/// position. Returns the blocks now in the section, the ids it had before, and whether the document
/// anchor was checked.
pub(super) fn apply_replace_section(
    doc: &mut ReportDoc,
    name: &str,
    markdown: &str,
    read: &SectionRead,
    doc_anchor: DocAnchor,
) -> Result<(Vec<ReportBlock>, Vec<String>, bool), CalmError> {
    let current = snapshot(doc)?;
    let (span, section, checked) = match locate_read(&current, name, read)? {
        Some(found) => (found.span.clone(), found.member_blocks(&current), false),
        None => {
            let at = declared_position(&current, name)?;
            // Creating a section places it among the others: a whole-document edit.
            (at..at, Vec::new(), check_doc_anchor(doc, doc_anchor, true)?)
        }
    };
    let marked = strip_markers_and_split(markdown);
    let opens_section = marked
        .slices
        .first()
        .is_some_and(|slice| h1_title(&slice.raw) == Some(name));
    if !opens_section
        || marked
            .slices
            .iter()
            .skip(1)
            .any(|slice| h1_title(&slice.raw).is_some())
    {
        return Err(CalmError::BadRequest(format!(
            "the markdown of a section replace must start with the line `# {name}` and hold no \
             other H1 line"
        )));
    }
    if let Some(outside) = marked
        .hints
        .iter()
        .flatten()
        .find(|id| !section.iter().any(|block| &block.id == *id))
    {
        return Err(CalmError::BadRequest(format!(
            "marker `<!-- neige:{outside} -->` names a block outside section `{name}`; a section \
             replace may pin only that section's own blocks"
        )));
    }
    if let Some(kind) = marked
        .slices
        .iter()
        .filter_map(|slice| parse_fence(&slice.raw))
        .map(|fence| fence.kind)
        .find(|kind| !in_sections(kind))
    {
        return Err(CalmError::BadRequest(format!(
            "the markdown of a section replace may not hold a `{kind}` block: it belongs to no \
             section — create or change it with an `upsert` op (no position), read it by id"
        )));
    }
    validate_body_fences(&marked.cleaned)?;
    let replaced = doc
        .replace_section_members(
            span,
            |block| in_sections(&block.kind),
            &marked.slices,
            &marked.hints,
        )
        .map_err(block_op_internal)?;
    let before = section.iter().map(|b| b.id.clone()).collect();
    Ok((replaced, before, checked))
}

/// Delete every member block of section `name` and return their ids; a task block in its span stays.
pub(super) fn apply_delete_section(
    doc: &mut ReportDoc,
    name: &str,
    read: &SectionRead,
) -> Result<Vec<String>, CalmError> {
    let current = snapshot(doc)?;
    let section = locate_read(&current, name, read)?.ok_or_else(|| {
        CalmError::BadRequest(section_error_message(
            &current,
            &crate::report_sections::SectionError::Unknown(name.to_string()),
        ))
    })?;
    let deleted: Vec<String> = section
        .member_blocks(&current)
        .into_iter()
        .map(|block| block.id.clone())
        .collect();
    for id in &deleted {
        doc.delete_block(id).map_err(block_op_internal)?;
    }
    Ok(deleted)
}

/// Where an absent section goes: before the first section the contract declares after it that is
/// present, else at the end. A section the contract does not declare is refused with the declared ones.
fn declared_position(blocks: &[ReportBlock], name: &str) -> Result<usize, CalmError> {
    let header = blocks
        .first()
        .map(flat_text)
        .and_then(|text| text.split('\n').next().and_then(parse_line))
        .and_then(Result::ok);
    let declared: Vec<&str> = header
        .as_ref()
        .map(|header| header.sections.iter().map(|s| s.h1.as_str()).collect())
        .unwrap_or_default();
    let Some(index) = declared.iter().position(|h1| *h1 == name) else {
        let listed = if declared.is_empty() {
            "this report's contract declares no sections".to_string()
        } else {
            format!(
                "its contract declares:\n{}",
                declared
                    .iter()
                    .map(|h1| format!("  # {h1}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };
        return Err(CalmError::BadRequest(format!(
            "section `{name}` does not exist and cannot be created: {listed}"
        )));
    };
    Ok(declared[index + 1..]
        .iter()
        .find_map(|later| {
            blocks
                .iter()
                .position(|block| block_h1_title(block) == Some(*later))
        })
        .unwrap_or(blocks.len()))
}
