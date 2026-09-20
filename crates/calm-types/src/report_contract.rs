//! The machine-readable `<!-- neige:contract {...} -->` header a kernel-assembled report body
//! carries as the FIRST line of block 0. v1 carries only `version` and `sections[{h1, omit_if_empty?}]`.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::report_blocks::split_body;

/// What a header line starts with. The trailing space is part of the prefix:
/// `<!-- neige:contract` with no JSON after it is not a header, and the block
/// marker lines (`<!-- neige:b_hhhh -->`) never collide with it.
pub const HEADER_OPEN: &str = "<!-- neige:contract ";

/// What a header line ends with (trailing whitespace tolerated by the parser, never produced by [`canonical_line`]).
pub const HEADER_CLOSE: &str = " -->";

/// The v1 contract header. Field order is the canonical JSON key order: this struct's layout IS the canonical form.
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
    /// The heading text after `# `. Non-empty, single-line, and not itself starting with `#`.
    pub h1: String,
    /// The section may be absent from the document when it has nothing to say.
    #[serde(default, skip_serializing_if = "is_false")]
    pub omit_if_empty: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Why a header (or a document carrying one) was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HeaderError {
    /// The line starts with [`HEADER_OPEN`] but is not a valid v1 header.
    #[error("malformed contract header: {0}")]
    Malformed(String),
    /// The document's one header is not on line 1. `line` is 1-based.
    #[error("contract header must be the first line of the document, found on line {line}")]
    Misplaced { line: usize },
    /// More than one line starts with [`HEADER_OPEN`].
    #[error("a document carries at most one contract header")]
    Duplicate,
    /// Block 0 (the contract block) ends inside an HTML comment, which would swallow the whole document on render.
    #[error("an HTML comment in the contract block is never closed")]
    ContractCommentUnclosed,
    /// A bug upstream of the funnel: a header that should have been normalized reached it non-canonical.
    #[error("internal: {0}")]
    Internal(String),
}

/// The one line every header is rewritten to: [`HEADER_OPEN`], the compact JSON, then [`HEADER_CLOSE`].
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

/// Recognise and validate one line (no line terminator): `None` when it is not a header at all,
/// `Some(Err(Malformed))` when it starts like one but is invalid, `Some(Ok)` for a valid header, canonical or not.
pub fn parse_line(line: &str) -> Option<Result<ContractHeader, HeaderError>> {
    let rest = line.strip_prefix(HEADER_OPEN)?;
    Some(parse_rest(rest))
}

fn parse_rest(rest: &str) -> Result<ContractHeader, HeaderError> {
    let header = parse_rest_once(rest)?;
    // The canonical form must parse back to the same header, or `normalize_header` would emit a line
    // its own parser rejects. `parse_rest_once`, not `parse_line`: the round trip must not recurse.
    let canonical = canonical_line(&header);
    let reparsed = canonical.strip_prefix(HEADER_OPEN).map(parse_rest_once);
    match reparsed {
        Some(Ok(same)) if same == header => Ok(header),
        other => Err(HeaderError::Malformed(format!(
            "the header does not survive canonicalisation: {canonical:?} re-parses as {other:?}"
        ))),
    }
}

/// One pass: syntax, then the value rules on the decoded header. No round trip.
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
    // Cheap first cut; a JSON-escaped `-->` only shows up once decoded, so the decoded values are checked again below.
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

/// The value-level rules, applied to the DECODED header (JSON escaping can hide any of these in the serialized text).
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

/// Line 1 of `body`, with everything from the first `\n` on (inclusive) as the second half.
fn split_first_line(body: &str) -> (&str, &str) {
    match body.find('\n') {
        Some(at) => (&body[..at], &body[at..]),
        None => (body, ""),
    }
}

/// Rewrite line 1 to its canonical form; looks at line 1 only and borrows when nothing changes.
/// A CRLF header line is rewritten to LF while the rest of the body keeps its endings.
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

/// The funnel check on a **marker-free** body: at most one header, on line 1, canonical, and block 0
/// does not end inside an HTML comment. Fence-unaware on purpose: a header quoted in a code fence still counts.
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

/// The block-0 comment scan: block 0 ending while an HTML comment is open is the defect. Deliberately
/// narrow and every narrowing is in the reject direction (no fence awareness, any leading blanks open).
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

/// Whether block 0 is nothing but HTML comments: after removing every `<!-- … -->` span, only
/// whitespace remains. An unclosed `<!--`, or one on a line indented as code, makes the block impure.
pub fn is_pure_comment_block(block0_raw: &str) -> bool {
    let mut at = 0;
    loop {
        let Some(open_rel) = block0_raw[at..].find("<!--") else {
            return block0_raw[at..].trim().is_empty();
        };
        let open_at = at + open_rel;
        if !block0_raw[at..open_at].trim().is_empty() || indented_as_code(block0_raw, open_at) {
            return false;
        }
        let after_open = open_at + "<!--".len();
        let Some(close_rel) = block0_raw[after_open..].find("-->") else {
            return false;
        };
        at = after_open + close_rel + "-->".len();
    }
}

/// Whether the `<!--` at byte `open_at` sits on a line CommonMark renders as an indented code block:
/// only spaces/tabs before it, at least four columns (any tab is enough).
fn indented_as_code(raw: &str, open_at: usize) -> bool {
    let line_start = raw[..open_at].rfind('\n').map_or(0, |newline| newline + 1);
    let leading = &raw[line_start..open_at];
    leading.bytes().all(|byte| byte == b' ' || byte == b'\t')
        && (leading.contains('\t') || leading.len() >= 4)
}

#[cfg(test)]
mod tests;
