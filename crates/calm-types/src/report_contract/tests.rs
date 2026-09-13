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
    // h1 multi-line (JSON-escaped newline decodes to a real one); `\r` too
    malformed(
        "<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"a\\nb\"}]} -->",
        "more than one line",
    );
    malformed(
        "<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"a\\rb\"}]} -->",
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

/// Review round 1 MAJOR: a literal `{"h1":"-->"}` is caught by the
/// serialized-text check, but an escaped spelling such as `{"h1":"-\u002d>"}`
/// passes it and decodes to `-->` — `canonical_line` would then emit a literal
/// `-->` inside the JSON, so `normalize_header` would produce a header its own
/// parser rejects. Both delimiters are rejected on the decoded value, so every
/// spelling is rejected the same way.
#[test]
fn h1_containing_a_comment_delimiter_is_malformed_even_when_json_escaped() {
    let malformed = |line: &str| match parse_line(line) {
        Some(Err(HeaderError::Malformed(message))) => {
            assert!(
                message.contains("comment delimiter"),
                "{line:?}: {message:?}"
            );
        }
        other => panic!("{line:?}: expected Malformed, got {other:?}"),
    };
    // Plain: the serialized text `{"h1":"-->"}` is followed by ` -->`, and
    // `rfind` takes the LAST close, so the first `-->` is inside the JSON
    // and caught by the serialized-text check — message differs; the
    // decoded check is what catches the escaped spellings below.
    assert!(matches!(
        parse_line("<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"-->\"}]} -->"),
        Some(Err(HeaderError::Malformed(_)))
    ));
    // JSON-escaped hyphen: `-\u002d>` decodes to `-->`.
    malformed("<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"a-\\u002d>b\"}]} -->");
    // `<!--` never trips the serialized check at all; decoded check only.
    malformed("<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"<!-- x\"}]} -->");
    malformed("<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"\\u003c!-- x\"}]} -->");
    // And the fully-escaped close: `\u002d\u002d\u003e`.
    malformed(
        "<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"\\u002d\\u002d\\u003e\"}]} -->",
    );
}

/// `normalize_header` is idempotent on every accepted input: what it emits
/// is canonical and parses back to the same header, so a second pass
/// borrows. Covers the canonical, the non-canonical spellings, escaped
/// unicode, and a header whose `h1` carries JSON-escaped characters that
/// survive (`"`, `\`, `/`).
#[test]
fn normalize_header_is_idempotent_on_every_accepted_input() {
    let inputs = [
        format!("{WORK_BRIEF_LINE}\n<!-- prose -->\n\n# 概要\n"),
        "<!-- neige:contract {\"sections\":[{\"h1\":\"概要\"},{\"omit_if_empty\":true,\"h1\":\"待你定\"}],\"version\":1} -->\nx\n".to_string(),
        "<!-- neige:contract { \"version\": 1, \"sections\": [ {\"h1\": \"a\", \"omit_if_empty\": false} ] }   -->   \n".to_string(),
        // escaped CJK decodes to the same header as the literal spelling
        "<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"\\u6982\\u8981\"}]} -->\n".to_string(),
        // characters serde_json escapes on the way back out
        "<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"say \\\"hi\\\" \\\\ a/b\"}]} -->\n".to_string(),
        // `-` and `>` are fine on their own; only the delimiter sequences are not
        "<!-- neige:contract {\"version\":1,\"sections\":[{\"h1\":\"a -> b -- c\"}]} -->\n".to_string(),
        WORK_BRIEF_LINE.to_string(),
    ];
    for input in inputs {
        let once = normalize_header(&input).unwrap_or_else(|e| panic!("{input:?}: {e}"));
        let twice = normalize_header(&once).unwrap();
        assert_eq!(once.as_ref(), twice.as_ref(), "{input:?}");
        assert!(
            matches!(twice, Cow::Borrowed(_)),
            "second pass must find line 1 canonical: {input:?}"
        );
        // and the funnel accepts the normalized form
        assert!(matches!(check_document(&once), Ok(Some(_))), "{input:?}");
    }
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
        check_document("<!-- neige:contract {\"version\":2,\"sections\":[{\"h1\":\"x\"}]} -->\n"),
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
