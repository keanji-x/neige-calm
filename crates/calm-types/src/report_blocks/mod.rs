//! Lossless markdown slicing and report-block identity reuse.
//!
//! * [`split_body`] / [`flatten`] — byte-exact slicing of the flat
//!   `body` projection: prose splits at unfenced H1/H2 headings, and
//!   (#960 PR3) a well-formed ```` ```neige-block <kind> ```` fence is
//!   cut out as its own non-prose slice ([`fence`]). The invariant
//!   `flatten(split_body(body)) == body` is byte-level.
//! * [`align`] (re-exported here) — LCS + similarity id reuse across
//!   wholesale rewrites; non-prose slices compare by their canonical
//!   fence text.
//! * [`kinds`] — the data-kind vocabulary + strict payload validation
//!   for write ends.
//! * marker helpers — the `<!-- neige:b_xxxx -->` id channel of
//!   `calm.report.write_markdown`.

pub mod fence;
pub mod kinds;
pub mod tasks;

mod align;

pub use align::{mint_id, reassign_ids, reassign_ids_with_hints};
pub use fence::{NonProseFence, canonical_json, neige_open_kind, parse_fence, render_fence};
pub use kinds::{
    DATA_KINDS, KIND_APP, KIND_CHART_CANDLES, KIND_PROSE, KIND_TABLE, KIND_TASK,
    MAX_CANONICAL_BYTES, MAX_CHART_CANDLES, MAX_STRING_CHARS, MAX_TABLE_COLUMNS, MAX_TABLE_ROWS,
    TASK_FIELDS, is_data_kind, scannable_text_fields, validate_payload,
};

use crate::wave_report::ReportBlock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSlice {
    pub raw: String,
}

/// Split at line-start ATX H1/H2 headings, except while inside a
/// fenced code block, and cut every well-formed `neige-block` fence
/// out as its own slice (prose before/after the fence goes to its
/// neighboring slices). Every input byte belongs to exactly one
/// returned slice. A slice is a non-prose block iff
/// [`fence::parse_fence`] accepts its `raw` — malformed `neige-block`
/// fences read as prose (lenient read; write ends must reject them
/// via [`invalid_neige_fences`]).
pub fn split_body(body: &str) -> Vec<BlockSlice> {
    scan(body).slices
}

/// Descriptions of every malformed `neige-block` fence in `body`: an
/// unindented ```` ```neige-block <kind> ```` opener whose region does
/// not parse (bad JSON, non-object payload, over-long/decorated closer,
/// unterminated), plus two near-miss typo shapes outside any fence —
/// a 1-3-space-indented `` ```neige-block `` opener, and a zero-indent
/// opener with trailing text after the kind. The lenient read treats
/// all of these as prose; write ends (`blocks.upsert` prose content,
/// `write_markdown`, the prose `Replace` shim) must refuse them so a
/// typo'd data block cannot be silently persisted as prose.
pub fn invalid_neige_fences(body: &str) -> Vec<String> {
    scan(body).invalid_fences
}

struct Scan {
    slices: Vec<BlockSlice>,
    invalid_fences: Vec<String>,
}

fn scan(body: &str) -> Scan {
    let mut starts = Vec::new();
    let mut invalid_fences = Vec::new();
    let mut offset = 0;
    // (marker, min close length, Some(open offset) iff neige candidate)
    let mut fence_state: Option<(u8, usize, Option<usize>)> = None;

    for line_with_ending in body.split_inclusive('\n') {
        let line_end = offset + line_with_ending.len();
        let line = line_with_ending
            .strip_suffix('\n')
            .unwrap_or(line_with_ending);
        let line = line.strip_suffix('\r').unwrap_or(line);

        if let Some((marker, minimum, neige_start)) = fence_state {
            let fence_line = strip_fence_indent(line);
            if fence_run(fence_line, marker)
                .is_some_and(|length| length >= minimum && fence_tail_is_blank(fence_line, length))
            {
                if let Some(start) = neige_start {
                    if fence::parse_fence(&body[start..line_end]).is_some() {
                        starts.push(start);
                        starts.push(line_end);
                    } else {
                        invalid_fences.push(malformed_fence_error(start));
                    }
                }
                fence_state = None;
            }
        } else if fence::neige_open_kind(line).is_some() {
            // A neige opener is also a plain backtick fence opener —
            // check it first so the candidate region is tracked.
            fence_state = Some((b'`', 3, Some(offset)));
        } else if let Some((marker, length)) = opening_fence(line) {
            // Typo'd neige openers (#960 PR3 review round 1): outside
            // any fence, a 1-3-space-indented `` ```neige-block ``
            // opener, or a zero-indent opener with trailing text after
            // the kind, is almost certainly a mistake — record it so
            // write ends reject with a fix hint. Examples inside an
            // outer fence (~~~ or 4-backtick) never reach this branch.
            if marker == b'`' {
                let stripped = strip_fence_indent(line);
                if stripped.starts_with("```neige-block") {
                    if stripped.len() != line.len() {
                        invalid_fences.push(format!(
                            "indented ```neige-block opener at byte {offset}: a neige-block \
                             fence must start at column 0 — remove the leading spaces (or wrap \
                             the snippet in a ~~~ fence if it is only an example)"
                        ));
                    } else if let Some(rest) = line.strip_prefix("```neige-block ")
                        && rest.trim_end().contains([' ', '\t'])
                    {
                        invalid_fences.push(format!(
                            "```neige-block opener with trailing text at byte {offset}: the \
                             info string must be exactly `neige-block <kind>` — remove \
                             everything after the kind"
                        ));
                    }
                }
            }
            fence_state = Some((marker, length, None));
        } else if is_h1_or_h2(line) {
            starts.push(offset);
        }
        offset = line_end;
    }
    if let Some((_, _, Some(start))) = fence_state {
        invalid_fences.push(format!(
            "unterminated ```neige-block fence starting at byte {start} (missing closing ``` line)"
        ));
    }

    if body.is_empty() {
        return Scan {
            slices: vec![BlockSlice { raw: String::new() }],
            invalid_fences,
        };
    }

    starts.sort_unstable();
    starts.dedup();
    starts.retain(|start| *start < body.len());
    if starts.first() != Some(&0) {
        starts.insert(0, 0);
    }
    let slices = starts
        .iter()
        .enumerate()
        .map(|(index, start)| BlockSlice {
            raw: body[*start..starts.get(index + 1).copied().unwrap_or(body.len())].to_string(),
        })
        .collect();
    Scan {
        slices,
        invalid_fences,
    }
}

fn malformed_fence_error(start: usize) -> String {
    format!(
        "malformed ```neige-block fence starting at byte {start}: the fence interior must be a \
         single JSON object and the opening/closing ``` lines must be unindented and undecorated"
    )
}

pub fn flatten(blocks: &[BlockSlice]) -> String {
    blocks.iter().map(|block| block.raw.as_str()).collect()
}

// ---------------------------------------------------------------------------
// Block-content rules — the ONE definition of "what a block may hold"
// ---------------------------------------------------------------------------
//
// Every write end that mints a block's stored content funnels through
// these three helpers. The write paths previously carried
// statement-for-statement copies of the same prose/data-kind rules,
// which would have drifted the first time a data kind or a fence rule
// changed. Only the *envelope* (how each surface finds `markdown` /
// `payload` in its own argument shape, and how it prefixes the
// resulting message) stays at the call site.

/// Prose content rule: markdown may not smuggle a `neige-block` fence,
/// well-formed or typo'd. A well-formed one would splinter into its own
/// block on the next wholesale write; a malformed one would silently
/// persist a broken data block as prose. Returns the message for the
/// first violation.
pub fn check_prose_markdown(markdown: &str) -> Result<(), String> {
    if let Some(first) = invalid_neige_fences(markdown).into_iter().next() {
        return Err(first);
    }
    if split_body(markdown)
        .iter()
        .any(|slice| parse_fence(&slice.raw).is_some())
    {
        return Err(
            "prose markdown may not embed a ```neige-block fence — create the data \
                    block with its own kind"
                .into(),
        );
    }
    Ok(())
}

/// Data-kind content rule: the payload is schema-validated and the
/// stored content is its canonical fence (deterministic pretty JSON).
/// `payload` must already be known to be an object — each surface words
/// that check in its own argument vocabulary.
pub fn render_data_block(kind: &str, payload: &serde_json::Value) -> Result<String, String> {
    validate_payload(kind, payload)
        .map_err(|errors| format!("invalid `{kind}` payload: {errors}"))?;
    Ok(render_fence(kind, payload))
}

/// The shared "this is not a block kind" message, so the supported-kind
/// list is enumerated in exactly one place.
pub fn unknown_kind_message(kind: &str) -> String {
    format!(
        "unknown kind `{kind}` — supported kinds: {}, {}",
        kinds::KIND_PROSE,
        kinds::DATA_KINDS.join(", ")
    )
}

/// A block's contribution to the flat `body` projection: prose blocks
/// contribute their markdown verbatim; non-prose blocks contribute
/// their canonical fence ([`fence::render_fence`] — deterministic, so
/// two documents differing only in a data block's parameters produce
/// different bodies and therefore different observation hashes).
pub fn flat_text(block: &ReportBlock) -> String {
    if block.kind == kinds::KIND_PROSE {
        block
            .payload
            .get("markdown")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    } else {
        fence::render_fence(&block.kind, &block.payload)
    }
}

fn is_h1_or_h2(line: &str) -> bool {
    line.starts_with("# ") || line.starts_with("## ")
}

fn fence_run(line: &str, marker: u8) -> Option<usize> {
    let length = line.bytes().take_while(|byte| *byte == marker).count();
    (length >= 3).then_some(length)
}

fn opening_fence(line: &str) -> Option<(u8, usize)> {
    let line = strip_fence_indent(line);
    b"`~".iter().copied().find_map(|marker| {
        fence_run(line, marker)
            .filter(|&length| marker != b'`' || !line[length..].contains('`'))
            .map(|length| (marker, length))
    })
}

fn strip_fence_indent(line: &str) -> &str {
    let indent = line
        .bytes()
        .take_while(|byte| *byte == b' ')
        .take(4)
        .count();
    if indent <= 3 { &line[indent..] } else { line }
}

fn fence_tail_is_blank(line: &str, run: usize) -> bool {
    line[run..].bytes().all(|byte| matches!(byte, b' ' | b'\t'))
}

/// A marker-stripped `write_markdown` body: the cleaned flat markdown
/// (guaranteed marker-free), its slices, and the per-slice id hints
/// recovered from the stripped marker lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkedBody {
    pub cleaned: String,
    pub slices: Vec<BlockSlice>,
    pub hints: Vec<Option<String>>,
}

/// Strip every standalone `<!-- neige:b_xxxx -->` marker line out of
/// `body` **unconditionally** (markers must never reach storage), then
/// split the cleaned text and bind each stripped id to the slice the
/// marker preceded. `hints` is index-aligned with `slices`; a slice
/// with no marker gets `None`, extra/trailing/duplicate markers are
/// dropped (first marker per slice wins). Feed the result to
/// [`reassign_ids_with_hints`].
pub fn strip_markers_and_split(body: &str) -> MarkedBody {
    let mut cleaned = String::with_capacity(body.len());
    let mut markers: Vec<(usize, String)> = Vec::new();
    for line in body.split_inclusive('\n') {
        match marker_line_id(line) {
            Some(id) => markers.push((cleaned.len(), id.to_string())),
            None => cleaned.push_str(line),
        }
    }
    let slices = split_body(&cleaned);
    let mut starts = Vec::with_capacity(slices.len());
    let mut offset = 0;
    for slice in &slices {
        starts.push(offset);
        offset += slice.raw.len();
    }
    let mut hints = vec![None; slices.len()];
    for (offset, id) in markers {
        if offset >= cleaned.len() && !cleaned.is_empty() {
            // Trailing marker with nothing after it: no block follows.
            continue;
        }
        // The slice whose byte range contains the position the marker
        // occupied — i.e. the block whose content directly follows it.
        let index = match starts.binary_search(&offset) {
            Ok(index) => index,
            Err(0) => 0,
            Err(insert) => insert - 1,
        };
        if hints[index].is_none() {
            hints[index] = Some(id);
        }
    }
    MarkedBody {
        cleaned,
        slices,
        hints,
    }
}

/// The exact marker line [`strip_markers_and_split`] strips and
/// `calm.report.read { with_markers: true }` injects.
pub fn marker_line(id: &str) -> String {
    format!("<!-- neige:{id} -->\n")
}

/// `Some(id)` iff `line` (one `split_inclusive('\n')` item) is a
/// standalone marker line: optional surrounding whitespace around
/// `<!-- neige:b_hhhh -->` with lowercase-hex `h`, nothing else.
fn marker_line_id(line: &str) -> Option<&str> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    let id = line
        .trim_matches([' ', '\t'])
        .strip_prefix("<!-- neige:")?
        .strip_suffix(" -->")?;
    (id.len() == 6
        && id.starts_with("b_")
        && id[2..]
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')))
    .then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    proptest! {
        #[test]
        fn split_and_flatten_is_byte_exact(body in any::<String>()) {
            prop_assert_eq!(flatten(&split_body(&body)), body);
        }

        #[test]
        fn markdown_state_machine_is_byte_exact(
            fragments in prop::collection::vec(
                prop::sample::select(vec![
                    "# A", "## B", "```", "~~~", "   ```", "    ```", "```x`y",
                    "text", "\n", "\r\n", "", "    indented", "\t", "\u{00a0}",
                    "```neige-block table", "```neige-block app", "{}",
                    "{\"src\": \"/x\"}", "not json", "````",
                ]),
                0..40,
            ),
        ) {
            let body = fragments.concat();
            prop_assert_eq!(flatten(&split_body(&body)), body);
        }
    }

    #[test]
    fn tricky_markdown_stays_lossless_and_only_real_headings_split() {
        let body = "preamble\r\n\r\n# A\r\n~~~md\r\n# fenced\r\n~~~\r\n    # indented\r\n> # quoted\r\n## B";
        let blocks = split_body(body);
        assert_eq!(flatten(&blocks), body);
        assert_eq!(blocks.len(), 3);
        assert!(blocks[1].raw.contains("# fenced"));
        assert!(blocks[1].raw.contains("# indented"));
        assert!(blocks[1].raw.contains("# quoted"));
    }

    #[test]
    fn empty_and_heading_only_documents_are_lossless() {
        for body in ["", "# A", "# A\n", "\r\n\r\n", "# A\r\n\r\n## B"] {
            assert_eq!(flatten(&split_body(body)), body);
        }
    }

    #[test]
    fn commonmark_fence_edges_split_only_real_headings() {
        let cases = [
            ("```\r\n# code\r\n```\r\n# real", 2),
            ("```\n# code\n```\n# real\n", 2),
            ("   ```rust\n# code\n   ```\n# real\n", 2),
            ("~~~rust\n# code\n~~~~~~\n# real\n", 2),
            ("   ~~~\n# code\n   ~~~\n# real\n", 2),
            ("    ```\n# real\n", 2),
            ("```foo`bar\n# A\n```\n# B\n", 2),
            ("```\n```\u{00a0}\n# still-code\n", 1),
            ("```\n# code\n# still-code", 1),
        ];

        for (body, expected_blocks) in cases {
            let blocks = split_body(body);
            assert_eq!(flatten(&blocks), body, "{body:?}");
            assert_eq!(blocks.len(), expected_blocks, "{body:?}");
        }

        assert!(
            split_body("```foo`bar\n# A\n```\n# B\n")[1]
                .raw
                .starts_with("# A\n")
        );
    }

    // -- neige-block fence slicing (#960 PR3) ------------------------

    #[test]
    fn well_formed_neige_fence_is_cut_as_its_own_slice() {
        let fence_text = "```neige-block app\n{\"src\": \"/x\"}\n```\n";
        let body = format!("# A\nprose before\n{fence_text}prose after\n# B\ntail\n");
        let blocks = split_body(&body);
        assert_eq!(flatten(&blocks), body);
        assert_eq!(blocks.len(), 4, "{blocks:?}");
        assert_eq!(blocks[0].raw, "# A\nprose before\n");
        assert_eq!(blocks[1].raw, fence_text);
        assert_eq!(blocks[2].raw, "prose after\n");
        assert_eq!(blocks[3].raw, "# B\ntail\n");
        // Slice ↔ fence recognition is one predicate: parse_fence
        // succeeds exactly on the fence slice.
        let parsed: Vec<bool> = blocks
            .iter()
            .map(|slice| parse_fence(&slice.raw).is_some())
            .collect();
        assert_eq!(parsed, [false, true, false, false]);
        assert!(invalid_neige_fences(&body).is_empty());

        // A fence directly at document start / end works too.
        let body = format!("{fence_text}# A\ntail");
        let blocks = split_body(&body);
        assert_eq!(flatten(&blocks), body);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].raw, fence_text);

        // Fence at EOF without trailing newline: still a block.
        let body = "# A\n```neige-block app\n{\"src\": \"/x\"}\n```";
        let blocks = split_body(body);
        assert_eq!(flatten(&blocks), body);
        assert_eq!(blocks.len(), 2);
        assert!(parse_fence(&blocks[1].raw).is_some());
    }

    #[test]
    fn malformed_neige_fences_read_as_prose_and_are_reported() {
        // Invalid JSON, unterminated, decorated closer, non-object.
        for body in [
            "# A\n```neige-block app\nnot json\n```\nrest\n",
            "# A\n```neige-block app\n{\"src\": \"/x\"}\n",
            "# A\n```neige-block app\n{}\n``` \nrest\n",
            "# A\n```neige-block app\n[1]\n```\nrest\n",
        ] {
            let blocks = split_body(body);
            assert_eq!(flatten(&blocks), body, "{body:?}");
            assert_eq!(blocks.len(), 1, "malformed fence stays prose: {body:?}");
            let invalid = invalid_neige_fences(body);
            assert_eq!(invalid.len(), 1, "{body:?} → {invalid:?}");
        }
        // Headings inside a malformed fence still do not split while
        // the fence is open (fence state machine unchanged) …
        let body = "```neige-block app\n# not a heading\nnot json\n```\n# real\n";
        let blocks = split_body(body);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1].raw, "# real\n");
        // … and an unterminated one swallows the rest.
        let body = "```neige-block app\n# not a heading\n";
        assert_eq!(split_body(body).len(), 1);
        assert_eq!(invalid_neige_fences(body).len(), 1);
    }

    #[test]
    fn neige_fence_inside_an_outer_fence_is_not_cut_and_not_invalid() {
        // Documentation showing the syntax inside ~~~ or 4-backtick
        // fences must be left alone — including indented or
        // trailing-text openers that would otherwise be typo-flagged.
        for body in [
            "~~~md\n```neige-block app\n{\"src\": \"/x\"}\n```\n~~~\n",
            "````md\n```neige-block app\nnot json\n```\n````\n",
            "~~~md\n  ```neige-block app\n~~~\n",
            "````md\n```neige-block app extra tail\n````\n",
        ] {
            let blocks = split_body(body);
            assert_eq!(flatten(&blocks), body, "{body:?}");
            assert_eq!(blocks.len(), 1, "{body:?}");
            assert!(invalid_neige_fences(body).is_empty(), "{body:?}");
        }
    }

    #[test]
    fn typo_neige_openers_are_flagged_for_write_ends() {
        // #960 PR3 review round 1: near-miss openers outside any fence
        // are almost certainly mistakes — flagged (write ends reject),
        // while the lenient read still treats them as prose.
        for (body, needle) in [
            // 1-3-space indent before the opener.
            (" ```neige-block app\n{\"src\": \"/x\"}\n```\n", "indented"),
            (
                "   ```neige-block app\n{\"src\": \"/x\"}\n   ```\n",
                "indented",
            ),
            // Zero-indent opener with trailing text after the kind.
            (
                "```neige-block app extra\n{\"src\": \"/x\"}\n```\n",
                "trailing text",
            ),
            (
                "```neige-block chart.candles day\n{}\n```\n",
                "trailing text",
            ),
        ] {
            let blocks = split_body(body);
            assert_eq!(flatten(&blocks), body, "{body:?}");
            assert_eq!(blocks.len(), 1, "typo fence reads as prose: {body:?}");
            let invalid = invalid_neige_fences(body);
            assert_eq!(invalid.len(), 1, "{body:?} → {invalid:?}");
            assert!(invalid[0].contains(needle), "{body:?} → {invalid:?}");
        }
        // Narrow judgment: shapes outside the two typo patterns stay
        // lenient (ordinary fences, nothing flagged).
        for body in [
            "```neige-block Chart\n{}\n```\n", // bad kind chars, no tail
            "```neige-blockapp\n{}\n```\n",    // no space after info word
            "````neige-block app\n{}\n````\n", // four backticks
            "    ```neige-block app\n",        // 4+ indent = indented code
        ] {
            assert!(
                invalid_neige_fences(body).is_empty(),
                "{body:?} must stay lenient"
            );
        }
    }

    #[test]
    fn flat_text_is_markdown_for_prose_and_canonical_fence_otherwise() {
        let prose = ReportBlock {
            id: "b_0001".into(),
            kind: "prose".into(),
            rev: 1,
            payload: json!({ "markdown": "# A\nalpha\n" }),
        };
        assert_eq!(flat_text(&prose), "# A\nalpha\n");
        let app = ReportBlock {
            id: "b_0002".into(),
            kind: "app".into(),
            rev: 1,
            payload: json!({ "src": "/x" }),
        };
        assert_eq!(
            flat_text(&app),
            "```neige-block app\n{\n  \"src\": \"/x\"\n}\n```\n"
        );
        // flat_text round-trips through the splitter as one fence slice.
        let blocks = split_body(&flat_text(&app));
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            parse_fence(&blocks[0].raw).unwrap().payload,
            json!({ "src": "/x" })
        );
    }

    // -- markers ------------------------------------------------------

    #[test]
    fn strip_markers_binds_ids_and_cleans_unconditionally() {
        let body = "<!-- neige:b_00aa -->\n# A\nalpha\n<!-- neige:b_00bb -->\n# B\nbeta\n";
        let marked = strip_markers_and_split(body);
        assert_eq!(marked.cleaned, "# A\nalpha\n# B\nbeta\n");
        assert!(!marked.cleaned.contains("<!-- neige:"));
        assert_eq!(marked.slices.len(), 2);
        assert_eq!(
            marked.hints,
            vec![Some("b_00aa".to_string()), Some("b_00bb".to_string())]
        );

        // Unknown / trailing / duplicate / CRLF / indented markers are
        // still stripped; binding is best-effort first-wins.
        let messy =
            "  <!-- neige:b_0001 -->  \r\n# A\n<!-- neige:b_0002 -->\nmid\n<!-- neige:b_0003 -->\n";
        let marked = strip_markers_and_split(messy);
        assert_eq!(marked.cleaned, "# A\nmid\n");
        assert_eq!(marked.slices.len(), 1);
        assert_eq!(marked.hints, vec![Some("b_0001".to_string())]);

        // Non-marker lookalikes stay in the body.
        for keep in [
            "x <!-- neige:b_0001 -->\n", // not standalone
            "<!-- neige:b_00G1 -->\n",   // non-hex
            "<!-- neige:b_00aaa -->\n",  // wrong length
            "<!-- neige:c_00aa -->\n",   // wrong prefix
            "<!--neige:b_00aa -->\n",    // malformed comment
            "<!-- neige:b_00AA -->\n",   // uppercase hex
        ] {
            let marked = strip_markers_and_split(keep);
            assert_eq!(marked.cleaned, keep, "{keep:?} must survive");
        }
    }

    #[test]
    fn markers_and_neige_fences_coexist() {
        // Marker line before a fence block binds to the fence slice;
        // marker before prose binds to prose. Cleaning never touches
        // the fence bytes.
        let fence_text = "```neige-block app\n{\"src\": \"/x\"}\n```\n";
        let body =
            format!("<!-- neige:b_00aa -->\n# A\nalpha\n<!-- neige:b_00bb -->\n{fence_text}");
        let marked = strip_markers_and_split(&body);
        assert_eq!(marked.cleaned, format!("# A\nalpha\n{fence_text}"));
        assert_eq!(marked.slices.len(), 2);
        assert_eq!(marked.slices[1].raw, fence_text);
        assert_eq!(
            marked.hints,
            vec![Some("b_00aa".to_string()), Some("b_00bb".to_string())]
        );
    }
}
