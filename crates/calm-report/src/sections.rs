//! H1 sections of a report (#1877): an H1 block plus every block after it up to the next H1, except
//! the blocks that belong to no section wherever they sit (task blocks, #2598).
//! A heading matches on its raw `# ` line, never on the H1/H2-blind outline heading.

use calm_types::report_blocks::{KIND_PROSE, in_sections};
use calm_types::track_report::ReportBlock;
use serde_json::Value;
use std::ops::Range;

/// The title of the section `text` opens: its first line is `# <title>`.
pub fn h1_title(text: &str) -> Option<&str> {
    let line = text.split('\n').next()?;
    line.strip_prefix("# ").map(str::trim_end)
}

/// The title of the section `block` opens, if it opens one (a prose block whose first line is `# `).
pub fn block_h1_title(block: &ReportBlock) -> Option<&str> {
    if block.kind != KIND_PROSE {
        return None;
    }
    block
        .payload
        .get("markdown")
        .and_then(Value::as_str)
        .and_then(h1_title)
}

/// One H1 section of a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section<'a> {
    pub title: &'a str,
    /// From its H1 block up to the next H1 block (or the end).
    pub span: Range<usize>,
    /// The indices in `span` of the blocks that belong to the section: every one but a task block.
    pub members: Vec<usize>,
}

impl Section<'_> {
    /// The member blocks of the section in document order; `blocks` is the list it was found in.
    pub fn member_blocks<'b>(&self, blocks: &'b [ReportBlock]) -> Vec<&'b ReportBlock> {
        self.members.iter().map(|index| &blocks[*index]).collect()
    }
}

/// Every section of `blocks` in document order.
pub fn sections(blocks: &[ReportBlock]) -> Vec<Section<'_>> {
    let starts: Vec<(usize, &str)> = blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| block_h1_title(block).map(|title| (index, title)))
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(n, (start, title))| {
            let end = starts.get(n + 1).map_or(blocks.len(), |(next, _)| *next);
            Section {
                title,
                span: *start..end,
                members: (*start..end)
                    .filter(|index| in_sections(&blocks[*index].kind))
                    .collect(),
            }
        })
        .collect()
}

/// Why a section name did not resolve to exactly one section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionError {
    Unknown(String),
    /// Several H1 blocks carry the name; the ids of their H1 blocks.
    Duplicate(String, Vec<String>),
}

/// The section titled `name`; `Ok(None)` when no section carries it.
pub fn find_section<'a>(
    blocks: &'a [ReportBlock],
    name: &str,
) -> Result<Option<Section<'a>>, SectionError> {
    let mut found = sections(blocks)
        .into_iter()
        .filter(|section| section.title == name)
        .collect::<Vec<_>>();
    match found.len() {
        0 => Ok(None),
        1 => Ok(found.pop()),
        _ => Err(SectionError::Duplicate(
            name.to_string(),
            found
                .iter()
                .map(|section| blocks[section.span.start].id.clone())
                .collect(),
        )),
    }
}

/// The ids of the member blocks of the sections `names` names, in document order; the input to the
/// one block renderer.
pub fn section_block_ids(
    blocks: &[ReportBlock],
    names: &[String],
) -> Result<Vec<String>, SectionError> {
    let mut chosen = vec![false; blocks.len()];
    for name in names {
        let section =
            find_section(blocks, name)?.ok_or_else(|| SectionError::Unknown(name.clone()))?;
        for index in section.members {
            chosen[index] = true;
        }
    }
    Ok(blocks
        .iter()
        .zip(chosen)
        .filter(|(_, chosen)| *chosen)
        .map(|(block, _)| block.id.clone())
        .collect())
}

/// The one refusal text for a section name, listing the report's H1 sections (or the candidates of
/// a duplicated one) so the caller can pick again.
pub fn section_error_message(blocks: &[ReportBlock], error: &SectionError) -> String {
    match error {
        SectionError::Unknown(name) => {
            let listed: Vec<String> = sections(blocks)
                .iter()
                .map(|section| format!("  # {}", section.title))
                .collect();
            if listed.is_empty() {
                format!("unknown section `{name}`; this report has no H1 sections")
            } else {
                format!(
                    "unknown section `{name}`; this report's sections are:\n{}",
                    listed.join("\n")
                )
            }
        }
        SectionError::Duplicate(name, ids) => format!(
            "section `{name}` is ambiguous: {} H1 blocks carry it ({}); address its blocks by id",
            ids.len(),
            ids.join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_types::report_blocks::{KIND_TASK, reassign_ids, render_fence, split_body};
    use serde_json::json;

    fn blocks(body: &str) -> Vec<ReportBlock> {
        reassign_ids(&[], &split_body(body))
    }

    #[test]
    fn a_section_runs_from_its_h1_to_the_next_h1_and_h2s_stay_inside() {
        let blocks = blocks("intro\n# A\na\n## A1\nx\n# B\nb\n#C\n");
        let found: Vec<_> = sections(&blocks)
            .into_iter()
            .map(|section| (section.title.to_string(), section.span, section.members))
            .collect();
        assert_eq!(
            found,
            vec![("A".into(), 1..3, vec![1, 2]), ("B".into(), 3..4, vec![3])]
        );
        assert_eq!(
            find_section(&blocks, "A").map(|found| found.map(|section| section.span)),
            Ok(Some(1..3))
        );
        assert_eq!(find_section(&blocks, "A1"), Ok(None), "an H2 is no section");
        let ids = section_block_ids(&blocks, &["B".into(), "A".into()]).unwrap();
        assert_eq!(
            ids,
            vec![
                blocks[1].id.clone(),
                blocks[2].id.clone(),
                blocks[3].id.clone()
            ]
        );
    }

    #[test]
    fn a_task_block_is_in_no_section_wherever_it_sits() {
        let task = |key: &str| render_fence(KIND_TASK, &json!({ "key": key }));
        let blocks = blocks(&format!(
            "{}# A\na\n{}## A1\nx\n# B\nb\n{}",
            task("t0"),
            task("t1"),
            task("t2")
        ));
        assert_eq!(blocks[2].kind, KIND_TASK, "premise: the task sits inside A");
        let found: Vec<_> = sections(&blocks)
            .into_iter()
            .map(|section| (section.span, section.members))
            .collect();
        assert_eq!(found, vec![(1..4, vec![1, 3]), (4..6, vec![4])]);
        let ids = section_block_ids(&blocks, &["A".into(), "B".into()]).unwrap();
        assert_eq!(
            ids,
            vec![
                blocks[1].id.clone(),
                blocks[3].id.clone(),
                blocks[4].id.clone()
            ]
        );
    }

    #[test]
    fn unknown_and_duplicate_names_are_refused_with_the_choices() {
        let dup = blocks("# A\na\n# A\nb\n# B\n");
        let err = find_section(&dup, "A").unwrap_err();
        assert_eq!(
            err,
            SectionError::Duplicate("A".into(), vec![dup[0].id.clone(), dup[1].id.clone()])
        );
        assert!(section_error_message(&dup, &err).contains(&dup[1].id));
        let err = section_block_ids(&dup, &["Z".into()]).unwrap_err();
        assert_eq!(
            section_error_message(&dup, &err),
            "unknown section `Z`; this report's sections are:\n  # A\n  # A\n  # B"
        );
    }
}
