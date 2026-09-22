//! Lossless markdown slicing and report-block identity reuse. The invariant
//! `flatten(split_body(body)) == body` is byte-level.

pub mod chart_series;
pub mod fence;
pub mod kinds;
pub mod tasks;

mod align;

#[cfg(test)]
mod projection_tests;

pub use align::{mint_id, reassign_ids, reassign_ids_with_hints};
pub use chart_series::{RANGE_DAYS, chart_series_range_days, is_valid_ymd, parse_ymd};
pub use fence::{NonProseFence, canonical_json, neige_open_kind, parse_fence, render_fence};
pub use kinds::{
    DATA_KINDS, KIND_APP, KIND_CHART_CANDLES, KIND_CHART_SERIES, KIND_LIVE_VIEW, KIND_PROSE,
    KIND_TABLE, KIND_TASK, MAX_CANONICAL_BYTES, MAX_CHART_CANDLES, MAX_CHART_SERIES,
    MAX_LIVE_VIEW_BYTES, MAX_STRING_CHARS, MAX_TABLE_COLUMNS, MAX_TABLE_ROWS, TASK_FIELDS,
    is_data_kind, scannable_text_fields, validate_payload,
};

use crate::track_report::ReportBlock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSlice {
    pub raw: String,
}

/// Split at line-start ATX H1/H2 headings, except while inside a fenced code block, and cut every
/// well-formed `neige-block` fence out as its own slice; malformed fences read as prose.
pub fn split_body(body: &str) -> Vec<BlockSlice> {
    scan(body).slices
}

/// Descriptions of every malformed `neige-block` fence in `body`, plus two near-miss typo shapes
/// outside any fence; the lenient read treats all of these as prose, write ends must refuse them.
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
            // A neige opener is also a plain backtick fence opener — check it first.
            fence_state = Some((b'`', 3, Some(offset)));
        } else if let Some((marker, length)) = opening_fence(line) {
            // Typo'd neige openers outside any fence (1-3-space indent, or trailing text after the kind) are
            // recorded so write ends reject them; examples inside an outer fence never reach this branch.
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

// Block-content rules — the ONE definition of "what a block may hold"; every write end funnels through these.

/// Prose content rule: markdown may not smuggle a `neige-block` fence, well-formed or typo'd.
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

/// Data-kind content rule: the payload is schema-validated and the stored content is its canonical fence.
pub fn render_data_block(kind: &str, payload: &serde_json::Value) -> Result<String, String> {
    validate_payload(kind, payload)
        .map_err(|errors| format!("invalid `{kind}` payload: {errors}"))?;
    Ok(render_fence(kind, payload))
}

/// The shared "this is not a block kind" message.
pub fn unknown_kind_message(kind: &str) -> String {
    format!(
        "unknown kind `{kind}` — supported kinds: {}, {}",
        kinds::KIND_PROSE,
        kinds::DATA_KINDS.join(", ")
    )
}

/// A block's contribution to the flat `body` projection: prose verbatim, non-prose its canonical fence.
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

/// Append an independent block to a flat projection, keeping its first line separate from an
/// unterminated preceding block; `["a", ""]` projects as `"a\n"`, while `["a"]` stays `"a"`.
pub fn append_block_text(body: &mut String, text: &str) {
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(text);
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

/// A marker-stripped `write_markdown` body: the cleaned flat markdown, its slices, and the per-slice id hints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkedBody {
    pub cleaned: String,
    pub slices: Vec<BlockSlice>,
    pub hints: Vec<Option<String>>,
}

/// Strip every standalone `<!-- neige:b_xxxx -->` marker line out of `body` **unconditionally**
/// (markers must never reach storage), then split and bind each stripped id to the slice it preceded.
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
            continue;
        }
        // The slice whose byte range contains the marker's position — the block that directly follows it.
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

/// The exact marker line [`strip_markers_and_split`] strips and `calm.report.read { with_markers: true }` injects.
pub fn marker_line(id: &str) -> String {
    format!("<!-- neige:{id} -->\n")
}

/// `Some(id)` iff `line` is a standalone marker line: optional surrounding whitespace around
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
        let parsed: Vec<bool> = blocks
            .iter()
            .map(|slice| parse_fence(&slice.raw).is_some())
            .collect();
        assert_eq!(parsed, [false, true, false, false]);
        assert!(invalid_neige_fences(&body).is_empty());

        let body = format!("{fence_text}# A\ntail");
        let blocks = split_body(&body);
        assert_eq!(flatten(&blocks), body);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].raw, fence_text);

        let body = "# A\n```neige-block app\n{\"src\": \"/x\"}\n```";
        let blocks = split_body(body);
        assert_eq!(flatten(&blocks), body);
        assert_eq!(blocks.len(), 2);
        assert!(parse_fence(&blocks[1].raw).is_some());
    }

    #[test]
    fn malformed_neige_fences_read_as_prose_and_are_reported() {
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
        let body = "```neige-block app\n# not a heading\nnot json\n```\n# real\n";
        let blocks = split_body(body);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1].raw, "# real\n");
        let body = "```neige-block app\n# not a heading\n";
        assert_eq!(split_body(body).len(), 1);
        assert_eq!(invalid_neige_fences(body).len(), 1);
    }

    #[test]
    fn neige_fence_inside_an_outer_fence_is_not_cut_and_not_invalid() {
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
        for (body, needle) in [
            (" ```neige-block app\n{\"src\": \"/x\"}\n```\n", "indented"),
            (
                "   ```neige-block app\n{\"src\": \"/x\"}\n   ```\n",
                "indented",
            ),
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
        let blocks = split_body(&flat_text(&app));
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            parse_fence(&blocks[0].raw).unwrap().payload,
            json!({ "src": "/x" })
        );
    }

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

        let messy =
            "  <!-- neige:b_0001 -->  \r\n# A\n<!-- neige:b_0002 -->\nmid\n<!-- neige:b_0003 -->\n";
        let marked = strip_markers_and_split(messy);
        assert_eq!(marked.cleaned, "# A\nmid\n");
        assert_eq!(marked.slices.len(), 1);
        assert_eq!(marked.hints, vec![Some("b_0001".to_string())]);

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
