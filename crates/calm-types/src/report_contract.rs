//! #1635 D2 — the machine-readable contract header a kernel-assembled report
//! body carries as the FIRST line of block 0.
//!
//! ```text
//! <!-- neige:contract {"version":1,"sections":[{"h1":"概要"},{"h1":"待你定","omit_if_empty":true},{"h1":"已完成"},{"h1":"决策"}]} -->
//! ```
//!
//! The header is how a report tells the kernel its own shape without the
//! kernel comparing section names: the prose maintenance contract that
//! follows it (`<!-- 报告维护契约 … -->`) is for the agent, the header is for
//! the code. This module owns the syntax and nothing else — it is not wired
//! into any write path here (S2c adds the four entry normalizations and the
//! funnel call; S3 rebuilds `report_startup_read_required` on top of it).
//!
//! **v1 carries only `version` and `sections[{h1, omit_if_empty?}]`.** Issue
//! #1635 D2 also lists `tasks` and `prose_budget?`; neither has a consumer, so
//! neither is in v1 — [`ContractHeader`] is `deny_unknown_fields`, and a header
//! that carries them (or any `version != 1`) parses as
//! [`HeaderError::Malformed`]. Adding a field is a `version` bump, not a
//! silent widening.
//!
//! Three operations, one canonical form:
//!
//! * [`parse_line`] — recognise a header line and validate it.
//! * [`normalize_header`] — rewrite line 1 to [`canonical_line`] so the funnel
//!   can compare bytes; a `Cow` because the common case (no header, or already
//!   canonical) borrows.
//! * [`check_document`] — the one-shot funnel check on a marker-free body:
//!   exactly one header, on line 1, canonical, and every HTML comment in
//!   block 0 closed.
//!
//! Plus [`is_pure_comment_block`], which S3's `is_unwritten` uses to decide
//! whether block 0 is "just the contract".

use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::report_blocks::split_body;

/// What a header line starts with. The trailing space is part of the prefix:
/// `<!-- neige:contract` with no JSON after it is not a header, and the block
/// marker lines (`<!-- neige:b_hhhh -->`) never collide with it.
pub const HEADER_OPEN: &str = "<!-- neige:contract ";

/// What a header line ends with (trailing whitespace tolerated by the parser,
/// never produced by [`canonical_line`]).
pub const HEADER_CLOSE: &str = " -->";

/// The v1 contract header. Field order is the canonical JSON key order —
/// `serde_json::to_string` serialises fields in declaration order, so this
/// struct's layout IS the canonical form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractHeader {
    /// Always `1` in v1; anything else is [`HeaderError::Malformed`].
    pub version: u32,
    /// The H1 sections the report is shaped by, in order. Never empty.
    pub sections: Vec<ContractSection>,
}

/// One declared section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractSection {
    /// The heading text after `# `. Non-empty, single-line, and not itself
    /// starting with `#` (a `## x` h1 would render as an H2 and split wrong).
    pub h1: String,
    /// The section may be absent from the document when it has nothing to
    /// say. Omitted from the canonical JSON when `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub omit_if_empty: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Why a header (or a document carrying one) was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HeaderError {
    /// The line starts with [`HEADER_OPEN`] but is not a valid v1 header:
    /// no ` -->`, trailing text after it, JSON that does not parse, JSON
    /// that contains `-->` (would close the HTML comment early), unknown
    /// fields, `version != 1`, no sections, or an `h1` that is empty,
    /// multi-line, or starts with `#`.
    #[error("malformed contract header: {0}")]
    Malformed(String),
    /// The document's one header is not on line 1. `line` is 1-based.
    #[error("contract header must be the first line of the document, found on line {line}")]
    Misplaced { line: usize },
    /// More than one line starts with [`HEADER_OPEN`].
    #[error("a document carries at most one contract header")]
    Duplicate,
    /// Block 0 (the contract block) ends inside an HTML comment: the comment
    /// would swallow the whole document on render (#1185).
    #[error("an HTML comment in the contract block is never closed")]
    ContractCommentUnclosed,
    /// A bug upstream of the funnel: a header that should have been
    /// normalized reached it non-canonical.
    #[error("internal: {0}")]
    Internal(String),
}

/// The one line every header is rewritten to: [`HEADER_OPEN`], the compact
/// JSON, then [`HEADER_CLOSE`]. Contains no `\n` (JSON string escaping
/// guarantees it; asserted in debug so a serialiser change cannot quietly
/// make a two-line header).
pub fn canonical_line(header: &ContractHeader) -> String {
    let json = serde_json::to_string(header)
        .expect("ContractHeader has only string-keyed, non-map fields; serialisation cannot fail");
    let line = format!("{HEADER_OPEN}{json}{HEADER_CLOSE}");
    debug_assert!(
        !line.contains('\n'),
        "a contract header must be one line: {line:?}"
    );
    line
}

/// Recognise and validate one line (no line terminator).
///
/// * `None` — the line does not start with [`HEADER_OPEN`]; it is not a
///   header at all.
/// * `Some(Err(Malformed))` — it does, but is not a valid v1 header. The
///   JSON is the text between [`HEADER_OPEN`] and the LAST ` -->` on the
///   line; trailing whitespace after that close is tolerated, anything else
///   is not.
/// * `Some(Ok(header))` — a valid header, canonical or not (callers that
///   care compare against [`canonical_line`]).
pub fn parse_line(line: &str) -> Option<Result<ContractHeader, HeaderError>> {
    let rest = line.strip_prefix(HEADER_OPEN)?;
    Some(parse_rest(rest))
}

fn parse_rest(rest: &str) -> Result<ContractHeader, HeaderError> {
    let Some(close_at) = rest.rfind(HEADER_CLOSE) else {
        return Err(HeaderError::Malformed(format!(
            "missing the closing `{HEADER_CLOSE}`"
        )));
    };
    let json = &rest[..close_at];
    let tail = &rest[close_at + HEADER_CLOSE.len()..];
    if !tail.trim().is_empty() {
        return Err(HeaderError::Malformed(format!(
            "trailing text after the closing `-->`: {tail:?}"
        )));
    }
    if json.contains("-->") {
        return Err(HeaderError::Malformed(
            "the header JSON contains `-->`, which would close the HTML comment early".into(),
        ));
    }
    let header: ContractHeader =
        serde_json::from_str(json).map_err(|error| HeaderError::Malformed(error.to_string()))?;
    if header.version != 1 {
        return Err(HeaderError::Malformed(format!(
            "unsupported contract version {} (this kernel speaks version 1)",
            header.version
        )));
    }
    if header.sections.is_empty() {
        return Err(HeaderError::Malformed(
            "`sections` must name at least one section".into(),
        ));
    }
    for section in &header.sections {
        if section.h1.is_empty() {
            return Err(HeaderError::Malformed("a section `h1` is empty".into()));
        }
        if section.h1.contains('\n') {
            return Err(HeaderError::Malformed(format!(
                "section `h1` {:?} spans more than one line",
                section.h1
            )));
        }
        if section.h1.starts_with('#') {
            return Err(HeaderError::Malformed(format!(
                "section `h1` {:?} must be the heading text, not the `# ` line",
                section.h1
            )));
        }
    }
    Ok(header)
}

/// Line 1 of `body`, with everything from the first `\n` on (inclusive) as
/// the second half. Line 1 is the whole body when there is no `\n`.
fn split_first_line(body: &str) -> (&str, &str) {
    match body.find('\n') {
        Some(at) => (&body[..at], &body[at..]),
        None => (body, ""),
    }
}

/// Rewrite line 1 to its canonical form. Looks at line 1 only.
///
/// * No header on line 1 → `Borrowed` (the body is handed back untouched;
///   [`check_document`] is what rejects a header further down).
/// * Header parses and is already [`canonical_line`] → `Borrowed`.
/// * Parses but is written differently (key order, spacing, an explicit
///   `"omit_if_empty":false`, trailing whitespace) → `Owned`, line 1
///   replaced, every other byte unchanged.
/// * Does not parse → the [`HeaderError::Malformed`] from [`parse_line`].
///
/// A `\r` before the `\n` counts as part of the line: it parses (trailing
/// whitespace) but is non-canonical, so a CRLF header line is rewritten to
/// LF while the rest of the body keeps whatever endings it had.
pub fn normalize_header(body: &str) -> Result<Cow<'_, str>, HeaderError> {
    let (first, rest) = split_first_line(body);
    match parse_line(first) {
        None => Ok(Cow::Borrowed(body)),
        Some(Err(error)) => Err(error),
        Some(Ok(header)) => {
            let canonical = canonical_line(&header);
            if first == canonical {
                Ok(Cow::Borrowed(body))
            } else {
                let mut out = String::with_capacity(canonical.len() + rest.len());
                out.push_str(&canonical);
                out.push_str(rest);
                Ok(Cow::Owned(out))
            }
        }
    }
}

/// The funnel check (#1635 D2 (a)–(e)) on a **marker-free** body — the
/// persisted flat projection. A `with_markers` read must be stripped by the
/// caller first; unstripped, the marker line makes the header line 2 and
/// this returns `Misplaced`, which is the intended fail-closed direction.
///
/// Every line is inspected, fence-unaware: a header line quoted inside a
/// code fence still counts (reject direction — a document that shows the
/// syntax must not be able to smuggle a second header).
///
/// * no line starts with [`HEADER_OPEN`] → `Ok(None)` (headerless bodies are
///   allowed; S3 treats them as legacy).
/// * more than one → [`HeaderError::Duplicate`].
/// * exactly one, not on line 1 → [`HeaderError::Misplaced`].
/// * exactly one, on line 1: it must parse ([`HeaderError::Malformed`]
///   propagates) and be byte-equal to [`canonical_line`] — a non-canonical
///   header here means an entry point skipped [`normalize_header`], which is
///   [`HeaderError::Internal`], not a user error. Then block 0 is scanned
///   for an unclosed HTML comment ([`HeaderError::ContractCommentUnclosed`]).
///
/// Block 0 is `split_body(body)[0].raw`, the same slice
/// `strip_markers_and_split(..).slices[0]` yields on a marker-free body.
pub fn check_document(body: &str) -> Result<Option<ContractHeader>, HeaderError> {
    let mut header_lines = body
        .split('\n')
        .enumerate()
        .filter(|(_, line)| line.starts_with(HEADER_OPEN));
    let Some((index, first)) = header_lines.next() else {
        return Ok(None);
    };
    if header_lines.next().is_some() {
        return Err(HeaderError::Duplicate);
    }
    if index != 0 {
        return Err(HeaderError::Misplaced { line: index + 1 });
    }
    let header = parse_line(first).unwrap_or_else(|| {
        Err(HeaderError::Internal(
            "a line that starts with HEADER_OPEN was not recognised as a header".into(),
        ))
    })?;
    if first != canonical_line(&header) {
        return Err(HeaderError::Internal(
            "non-canonical header reached the funnel".into(),
        ));
    }
    let slices = split_body(body);
    if block0_ends_inside_a_comment(&slices[0].raw) {
        return Err(HeaderError::ContractCommentUnclosed);
    }
    Ok(Some(header))
}

/// The block-0 comment scan (#1635 D2 (e), v5 descope — do not widen).
///
/// Walks the lines of block 0 in order. A line whose text after leading
/// blanks starts with `<!--` opens a comment; the opening line itself closes
/// it if `-->` appears after that `<!--`; otherwise the next line containing
/// `-->` closes it. Block 0 ending while a comment is open is the defect
/// (#1185: the kernel-shipped top contract losing its `-->` swallows the whole
/// document on render).
///
/// Deliberately narrow, and every narrowing is in the reject direction:
///
/// * no fence awareness — a `<!--` inside a code fence in block 0 also opens;
/// * any number of leading blanks — `    <!--` (4 spaces, indented code in
///   CommonMark) also opens, as does `   <!--` (3 spaces);
/// * only block 0 is ever scanned; comments in blocks ≥ 1 are the user's.
///
/// Consequence worth knowing (issue §6.5): a stray `-->` in block 0 that is
/// not inside a comment is ignored, and a `-->` inside a fence (a mermaid
/// arrow, say) closes an open comment at the same place CommonMark would.
fn block0_ends_inside_a_comment(block0: &str) -> bool {
    let mut open = false;
    for line in block0.split('\n') {
        if open {
            if line.contains("-->") {
                open = false;
            }
            continue;
        }
        let text = line.trim_start_matches([' ', '\t']);
        if let Some(after_open) = text.strip_prefix("<!--") {
            open = !after_open.contains("-->");
        }
    }
    open
}

/// Whether block 0 is nothing but HTML comments: after removing every
/// `<!-- … -->` span (fence-unaware, leftmost-first, non-nesting), only
/// whitespace remains. An unclosed `<!--` is not a span and therefore makes
/// the block impure. S3's `is_unwritten` uses this to decide that block 0 of
/// a headered body is "just the contract".
pub fn is_pure_comment_block(block0_raw: &str) -> bool {
    let mut rest = block0_raw;
    loop {
        let Some(open_at) = rest.find("<!--") else {
            return rest.trim().is_empty();
        };
        if !rest[..open_at].trim().is_empty() {
            return false;
        }
        let after_open = &rest[open_at + "<!--".len()..];
        let Some(close_at) = after_open.find("-->") else {
            return false;
        };
        rest = &after_open[close_at + "-->".len()..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(sections: &[(&str, bool)]) -> ContractHeader {
        ContractHeader {
            version: 1,
            sections: sections
                .iter()
                .map(|(h1, omit_if_empty)| ContractSection {
                    h1: (*h1).to_string(),
                    omit_if_empty: *omit_if_empty,
                })
                .collect(),
        }
    }

    fn work_brief() -> ContractHeader {
        header(&[
            ("概要", false),
            ("待你定", true),
            ("已完成", false),
            ("决策", false),
        ])
    }

    const WORK_BRIEF_LINE: &str = "<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"概要\"},{\"h1\":\"待你定\",\"omit_if_empty\":true},{\"h1\":\"已完成\"},{\"h1\":\"决策\"}]} -->";

    // —— canonical form ————————————————————————————————————————————————

    #[test]
    fn canonical_line_is_the_documented_one_liner_and_round_trips() {
        let line = canonical_line(&work_brief());
        assert_eq!(line, WORK_BRIEF_LINE);
        assert!(!line.contains('\n'));
        assert_eq!(parse_line(&line), Some(Ok(work_brief())));
        // `omit_if_empty: false` is omitted, `true` is kept.
        assert!(!line.contains("\"omit_if_empty\":false"));
        assert_eq!(line.matches("omit_if_empty").count(), 1);
    }

    #[test]
    fn non_canonical_spellings_parse_to_the_same_header_and_normalize() {
        let spellings = [
            // key order
            "<!-- neige:contract {\"sections\":[{\"h1\":\"概要\"},{\"omit_if_empty\":true,\"h1\":\"待你定\"},{\"h1\":\"已完成\"},{\"h1\":\"决策\"}],\"version\":1} -->",
            // whitespace inside the JSON
            "<!-- neige:contract { \"version\": 1, \"sections\": [ {\"h1\": \"概要\"}, {\"h1\": \"待你定\", \"omit_if_empty\": true}, {\"h1\": \"已完成\"}, {\"h1\": \"决策\"} ] } -->",
            // explicit false
            "<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"概要\",\"omit_if_empty\":false},{\"h1\":\"待你定\",\"omit_if_empty\":true},{\"h1\":\"已完成\"},{\"h1\":\"决策\"}]} -->",
            // trailing whitespace after the close
            "<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"概要\"},{\"h1\":\"待你定\",\"omit_if_empty\":true},{\"h1\":\"已完成\"},{\"h1\":\"决策\"}]} -->   ",
        ];
        for spelling in spellings {
            assert_eq!(parse_line(spelling), Some(Ok(work_brief())), "{spelling}");
            let body = format!("{spelling}\n<!-- prose -->\n\n# 概要\n");
            let normalized = normalize_header(&body).unwrap();
            assert!(matches!(normalized, Cow::Owned(_)), "{spelling}");
            assert_eq!(
                normalized.as_ref(),
                format!("{WORK_BRIEF_LINE}\n<!-- prose -->\n\n# 概要\n"),
                "{spelling}"
            );
        }
    }

    #[test]
    fn normalize_header_borrows_when_there_is_nothing_to_do() {
        for body in [
            "",
            "# 概要\n",
            "<!-- prose only -->\n\n# 概要\n",
            // a header further down is not line 1's business
            "prose\n<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"x\"}]} -->\n",
            // a marker line is not a header
            "<!-- neige:b_00aa -->\n# A\n",
        ] {
            assert!(
                matches!(normalize_header(body), Ok(Cow::Borrowed(_))),
                "{body:?}"
            );
        }
        let canonical = format!("{WORK_BRIEF_LINE}\n\n# 概要\n");
        assert!(matches!(normalize_header(&canonical), Ok(Cow::Borrowed(_))));
        // No trailing newline at all: line 1 is the whole body.
        assert!(matches!(
            normalize_header(WORK_BRIEF_LINE),
            Ok(Cow::Borrowed(_))
        ));
    }

    #[test]
    fn normalize_header_rewrites_only_line_one() {
        let body =
            "<!-- neige:contract {\"sections\":[{\"h1\":\"x\"}],\"version\":1} -->\r\n\r\n# x\r\n";
        let normalized = normalize_header(body).unwrap();
        assert_eq!(
            normalized.as_ref(),
            "<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"x\"}]} -->\n\r\n# x\r\n",
            "line 1 goes canonical (LF), every later byte is untouched"
        );
        assert!(matches!(
            normalize_header("<!-- neige:contract {} -->\n"),
            Err(HeaderError::Malformed(_))
        ));
    }

    // —— parse_line negatives, one per rule ————————————————————————————

    #[test]
    fn parse_line_is_none_for_non_header_lines() {
        for line in [
            "",
            "# 概要",
            "<!-- 报告维护契约",
            "<!-- neige:b_00aa -->",
            "<!-- neige:contract", // no trailing space → not the prefix
            "<!--neige:contract {\"version\":1,\"sections\":[{\"h1\":\"x\"}]} -->",
            " <!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"x\"}]} -->",
        ] {
            assert_eq!(parse_line(line), None, "{line:?}");
        }
    }

    #[test]
    fn parse_line_rejects_one_rule_at_a_time() {
        let malformed = |line: &str, needle: &str| match parse_line(line) {
            Some(Err(HeaderError::Malformed(message))) => assert!(
                message.contains(needle),
                "{line:?}: expected {needle:?} in {message:?}"
            ),
            other => panic!("{line:?}: expected Malformed, got {other:?}"),
        };
        // no close at all
        malformed(
            "<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"x\"}]}",
            "missing the closing",
        );
        // `-->` without the leading space is not HEADER_CLOSE
        malformed(
            "<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"x\"}]}-->",
            "missing the closing",
        );
        // trailing non-whitespace after the close
        malformed(
            "<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"x\"}]} --> tail",
            "trailing text",
        );
        // JSON does not parse
        malformed("<!-- neige:contract not json -->", "expected");
        malformed("<!-- neige:contract  -->", "EOF");
        // JSON contains `-->` (the last ` -->` is the close; the inner one
        // would end the HTML comment early)
        malformed(
            "<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"a --> b\"}]} -->",
            "contains `-->`",
        );
        // unknown field: the issue's `tasks` / `prose_budget` are not v1
        malformed(
            "<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"x\"}],\"tasks\":true} -->",
            "unknown field `tasks`",
        );
        malformed(
            "<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"x\"}],\"prose_budget\":1000} -->",
            "unknown field `prose_budget`",
        );
        malformed(
            "<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"x\",\"omit\":true}]} -->",
            "unknown field `omit`",
        );
        // missing required field
        malformed(
            "<!-- neige:contract {\"sections\":[{\"h1\":\"x\"}]} -->",
            "missing field `version`",
        );
        malformed(
            "<!-- neige:contract {\"version\":1} -->",
            "missing field `sections`",
        );
        // version != 1
        malformed(
            "<!-- neige:contract {\"version\":2,\"sections\":[{\"h1\":\"x\"}]} -->",
            "unsupported contract version 2",
        );
        malformed(
            "<!-- neige:contract {\"version\":0,\"sections\":[{\"h1\":\"x\"}]} -->",
            "unsupported contract version 0",
        );
        // sections empty
        malformed(
            "<!-- neige:contract {\"version\":1,\"sections\":[]} -->",
            "at least one section",
        );
        // h1 empty
        malformed(
            "<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"\"}]} -->",
            "`h1` is empty",
        );
        // h1 multi-line (JSON-escaped newline decodes to a real one)
        malformed(
            "<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"a\\nb\"}]} -->",
            "more than one line",
        );
        // h1 starts with `#`
        malformed(
            "<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"# 概要\"}]} -->",
            "not the `# ` line",
        );
        malformed(
            "<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"#x\"}]} -->",
            "not the `# ` line",
        );
    }

    // —— check_document: header placement ——————————————————————————————

    #[test]
    fn check_document_accepts_headerless_bodies() {
        for body in [
            "",
            "![chart](x)",
            "![chart](x)\n",
            "# 概要\n\ntext\n",
            "<!-- prose -->\n",
        ] {
            assert_eq!(check_document(body), Ok(None), "{body:?}");
        }
    }

    #[test]
    fn check_document_accepts_a_canonical_header_on_line_one() {
        assert_eq!(check_document(WORK_BRIEF_LINE), Ok(Some(work_brief())));
        assert_eq!(
            check_document(&format!("{WORK_BRIEF_LINE}\n")),
            Ok(Some(work_brief()))
        );
        assert_eq!(
            check_document(&format!("{WORK_BRIEF_LINE}\n\n# 概要\n\n# 待你定\n")),
            Ok(Some(work_brief()))
        );
    }

    #[test]
    fn check_document_rejects_a_misplaced_header() {
        assert_eq!(
            check_document(&format!("\n{WORK_BRIEF_LINE}\n")),
            Err(HeaderError::Misplaced { line: 2 })
        );
        assert_eq!(
            check_document(&format!("# 概要\n\ntext\n{WORK_BRIEF_LINE}\n")),
            Err(HeaderError::Misplaced { line: 4 })
        );
        // The exact with-markers shape S2c must strip before calling: the
        // marker line pushes the header to line 2 → fail closed.
        assert_eq!(
            check_document(&format!("<!-- neige:b_00aa -->\n{WORK_BRIEF_LINE}\n")),
            Err(HeaderError::Misplaced { line: 2 })
        );
        // Fence-unaware: a header quoted inside a fence still counts.
        assert_eq!(
            check_document(&format!("# 概要\n\n```\n{WORK_BRIEF_LINE}\n```\n")),
            Err(HeaderError::Misplaced { line: 4 })
        );
    }

    #[test]
    fn check_document_rejects_duplicate_headers_before_anything_else() {
        assert_eq!(
            check_document(&format!("{WORK_BRIEF_LINE}\n{WORK_BRIEF_LINE}\n")),
            Err(HeaderError::Duplicate)
        );
        // Duplicate wins over Misplaced, and over the second one being
        // malformed or fenced.
        assert_eq!(
            check_document(&format!(
                "x\n{WORK_BRIEF_LINE}\n```\n<!-- neige:contract junk\n```\n"
            )),
            Err(HeaderError::Duplicate)
        );
    }

    #[test]
    fn check_document_propagates_malformed_and_flags_non_canonical_as_internal() {
        assert!(matches!(
            check_document(
                "<!-- neige:contract {\"version\":2,\"sections\":[{\"h1\":\"x\"}]} -->\n"
            ),
            Err(HeaderError::Malformed(_))
        ));
        let reordered = "<!-- neige:contract {\"sections\":[{\"h1\":\"概要\"},{\"h1\":\"待你定\",\"omit_if_empty\":true},{\"h1\":\"已完成\"},{\"h1\":\"决策\"}],\"version\":1} -->\n";
        assert_eq!(
            check_document(reordered),
            Err(HeaderError::Internal(
                "non-canonical header reached the funnel".into()
            ))
        );
        // …and the fix is exactly `normalize_header` first.
        let normalized = normalize_header(reordered).unwrap();
        assert_eq!(check_document(&normalized), Ok(Some(work_brief())));
    }

    // —— check_document: block-0 comment scan (D2 (e), v5 descope) ———————

    fn with_block0(tail: &str) -> String {
        format!("{WORK_BRIEF_LINE}\n{tail}")
    }

    #[test]
    fn block0_header_only_and_closed_comments_are_accepted() {
        // header only
        assert_eq!(check_document(&with_block0("")), Ok(Some(work_brief())));
        // header + two single-line comments
        assert_eq!(
            check_document(&with_block0("<!-- one -->\n<!-- two -->\n\n# 概要\n")),
            Ok(Some(work_brief()))
        );
        // header + a closed multi-line prose comment (the shipped shape)
        assert_eq!(
            check_document(&with_block0(
                "<!-- 报告维护契约\n\n这份报告自带的结构就是规则。\n-->\n\n# 概要\n\ntext\n"
            )),
            Ok(Some(work_brief()))
        );
        // `-->` on the same line as a later `<!--` closes that one too
        assert_eq!(
            check_document(&with_block0("<!-- a -->\n<!-- b\nstill b\n-->\n\n# 概要\n")),
            Ok(Some(work_brief()))
        );
    }

    #[test]
    fn block0_unclosed_prose_comment_is_rejected() {
        assert_eq!(
            check_document(&with_block0(
                "<!-- 报告维护契约\n\n这份报告自带的结构就是规则。\n\n# 概要\n\ntext\n"
            )),
            Err(HeaderError::ContractCommentUnclosed),
            "the #1185 hazard: the top contract lost its `-->`"
        );
        // Unclosed on the very last line of block 0, no trailing newline.
        assert_eq!(
            check_document(&with_block0("<!-- open")),
            Err(HeaderError::ContractCommentUnclosed)
        );
    }

    #[test]
    fn block0_indented_openers_open_regardless_of_depth() {
        // 3 spaces: an HTML block in CommonMark, opens.
        assert_eq!(
            check_document(&with_block0("   <!-- open\n\n# 概要\n")),
            Err(HeaderError::ContractCommentUnclosed)
        );
        // 4 spaces: indented code in CommonMark — ALSO opens (reject direction).
        assert_eq!(
            check_document(&with_block0("    <!-- open\n\n# 概要\n")),
            Err(HeaderError::ContractCommentUnclosed)
        );
        // …and both close normally.
        assert_eq!(
            check_document(&with_block0("   <!-- a\n-->\n    <!-- b -->\n\n# 概要\n")),
            Ok(Some(work_brief()))
        );
    }

    #[test]
    fn block0_opener_inside_a_fence_still_opens() {
        // Reject direction pinned: no fence awareness in the scan.
        assert_eq!(
            check_document(&with_block0(
                "```\n<!-- shown as an example\n```\n\n# 概要\n"
            )),
            Err(HeaderError::ContractCommentUnclosed)
        );
    }

    #[test]
    fn blocks_after_zero_are_never_scanned() {
        // An unclosed comment in block 1 is the user's business (v5 descope).
        assert_eq!(
            check_document(&with_block0(
                "<!-- ok -->\n\n# 概要\n\n<!-- never closed\n\n# 待你定\n"
            )),
            Ok(Some(work_brief()))
        );
        // Block 0 closed, block ≥1 opens and never closes — still Ok.
        assert_eq!(
            check_document(&with_block0("\n# A\n<!--\n")),
            Ok(Some(work_brief()))
        );
    }

    /// Mutation proof (#1635 S2b brief): replacing the ordered scan with a
    /// pair count (`matches("<!--").count() == matches("-->").count()`) must
    /// go red here. Opens 2 / closes 2, yet the last comment is open.
    #[test]
    fn stray_close_before_an_open_does_not_balance_it() {
        let body = with_block0("-->\n<!-- unclosed\n");
        assert_eq!(body.matches("<!--").count(), 2);
        assert_eq!(body.matches("-->").count(), 2);
        assert_eq!(
            check_document(&body),
            Err(HeaderError::ContractCommentUnclosed)
        );
    }

    #[test]
    fn scan_is_header_gated_by_construction() {
        // With no header there is nothing to scan: the same unclosed block 0
        // is Ok(None). (Reads as legacy/written in S3; not this module's call.)
        assert_eq!(check_document("<!-- never closed\n\n# 概要\n"), Ok(None));
    }

    // —— is_pure_comment_block ————————————————————————————————————————

    #[test]
    fn pure_comment_blocks_are_comments_and_whitespace_only() {
        for pure in [
            "",
            "   \n\n",
            WORK_BRIEF_LINE,
            &format!("{WORK_BRIEF_LINE}\n"),
            &format!("{WORK_BRIEF_LINE}\n<!-- 报告维护契约\n\n规则。\n-->\n\n"),
            "<!-- a --><!-- b -->\n",
            "<!-- a -->\n\n<!-- b\nc\n-->\n",
        ] {
            assert!(is_pure_comment_block(pure), "{pure:?}");
        }
        for impure in [
            "x",
            "<!-- a --> x",
            "x <!-- a -->",
            "<!-- unclosed",
            "<!-- a -->\n# 概要\n",
            "<!-- a -->\n![chart](x)\n",
            "-->",
            "<!-- a -->\n<!-- b --> tail\n",
        ] {
            assert!(!is_pure_comment_block(impure), "{impure:?}");
        }
    }

    #[test]
    fn errors_display_their_shape() {
        assert_eq!(
            HeaderError::Misplaced { line: 2 }.to_string(),
            "contract header must be the first line of the document, found on line 2"
        );
        assert!(
            HeaderError::Malformed("x".into())
                .to_string()
                .contains("malformed")
        );
        assert!(HeaderError::Duplicate.to_string().contains("at most one"));
        assert!(
            HeaderError::ContractCommentUnclosed
                .to_string()
                .contains("never closed")
        );
        assert!(
            HeaderError::Internal("x".into())
                .to_string()
                .starts_with("internal: ")
        );
    }
}
