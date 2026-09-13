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
//! into any write path here (S2c added the four entry normalizations and the
//! funnel call; S3 rebuilt `report_startup_read_required` on top of it).
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
//!   at most one header, on line 1, canonical, and block 0 does not end
//!   inside an HTML comment (line-based scan, see
//!   `block0_ends_inside_a_comment`; it runs only when a header is present —
//!   headerless bodies return `Ok(None)` before it).
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
    /// fields, `version != 1`, no sections, an `h1` that is empty,
    /// multi-line (`\n` or `\r`), starts with `#`, or contains `-->` /
    /// `<!--` once decoded, or a header whose canonical form does not parse
    /// back to itself.
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
///   is not. The value rules are checked on the decoded header, and the
///   header must survive `canonical_line` → `parse_line` unchanged.
/// * `Some(Ok(header))` — a valid header, canonical or not (callers that
///   care compare against [`canonical_line`]). `normalize_header` is
///   idempotent on every such header.
pub fn parse_line(line: &str) -> Option<Result<ContractHeader, HeaderError>> {
    let rest = line.strip_prefix(HEADER_OPEN)?;
    Some(parse_rest(rest))
}

fn parse_rest(rest: &str) -> Result<ContractHeader, HeaderError> {
    let header = parse_rest_once(rest)?;
    // The canonical form must parse back to the same header, or
    // `normalize_header` would emit a line its own parser rejects (a decoded
    // `-->` re-serialises as a literal `-->` inside the JSON). Every accepted
    // header is therefore idempotent under normalisation by construction.
    // `parse_rest_once`, not `parse_line`: the round trip must not recurse.
    let canonical = canonical_line(&header);
    let reparsed = canonical.strip_prefix(HEADER_OPEN).map(parse_rest_once);
    match reparsed {
        Some(Ok(same)) if same == header => Ok(header),
        other => Err(HeaderError::Malformed(format!(
            "the header does not survive canonicalisation: {canonical:?} re-parses as {other:?}"
        ))),
    }
}

/// One pass: syntax, then the value rules on the decoded header. No round
/// trip (that is [`parse_rest`]'s job, and it calls this twice).
fn parse_rest_once(rest: &str) -> Result<ContractHeader, HeaderError> {
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
    // Cheap first cut on the serialized text. Not sufficient on its own: a
    // JSON-escaped `-->` (`"-\u002d>"`) passes here and only shows up once
    // decoded, so the decoded values are checked again below.
    if json.contains("-->") {
        return Err(HeaderError::Malformed(
            "the header JSON contains `-->`, which would close the HTML comment early".into(),
        ));
    }
    let header: ContractHeader =
        serde_json::from_str(json).map_err(|error| HeaderError::Malformed(error.to_string()))?;
    validate_decoded(&header)?;
    Ok(header)
}

/// The value-level rules, applied to the DECODED header (JSON escaping means
/// the serialized text can hide any of these).
fn validate_decoded(header: &ContractHeader) -> Result<(), HeaderError> {
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
        if section.h1.contains(['\n', '\r']) {
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
        if section.h1.contains("-->") || section.h1.contains("<!--") {
            return Err(HeaderError::Malformed(format!(
                "section `h1` {:?} contains an HTML comment delimiter, which would \
                 close or reopen the header comment once serialised",
                section.h1
            )));
        }
    }
    Ok(())
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
mod tests;
