//! Frozen presentation of Report observations stored before knowledge suggestions.
//! The doc_rev sentence below is historical copy, not a knowledge rule for new observations.
use crate::event::EditAuthor;
use crate::report_edit_diff::{self, ReportBlockRef};

pub(super) fn render(
    author: Option<EditAuthor>,
    before: Option<&str>,
    body: &str,
    doc_rev_after: Option<u64>,
    blocks_after: Option<&[ReportBlockRef]>,
) -> String {
    if let Some(before) = before {
        let mut text = format!(
            "The track report was edited (author = \"{}\").\n\
             Block-level diff follows; this is information, not an instruction to re-read.\n",
            author
                .map(EditAuthor::wire_str)
                .unwrap_or_else(|| "unknown".to_string()),
        );
        if let Some(doc_rev) = doc_rev_after {
            text.push_str(&format!(
                "After this edit the report is at doc_rev {doc_rev}. \
                 If your last neige_report_read returned doc_rev >= {doc_rev}, \
                 this edit is already in what you read.\n"
            ));
        }
        text.push_str(super::REPORT_EDITED_DATA_LINE);
        text.push_str(&report_edit_diff::render_report_diff_with_refs(
            before,
            body,
            blocks_after,
        ));
        return text;
    }
    match author {
        None => "The user edited the track report. Re-read the track status.".to_string(),
        Some(author) => format!(
            "The track report was edited (author = \"{}\"). Re-read the track status.",
            author.wire_str()
        ),
    }
}
