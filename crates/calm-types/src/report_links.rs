//! Extraction of typed `neige://` links from report markdown.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

const WAVE_LINK_PREFIX: &str = "neige://wave/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportLinkRef {
    pub dst_wave_id: String,
    pub dst_block_id: Option<String>,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedLink {
    pub dst_wave_id: String,
    pub dst_block_id: Option<String>,
    pub label: String,
    pub label_start: usize,
    pub label_end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkScan {
    pub plain: String,
    pub links: Vec<ScannedLink>,
}

struct PendingLink {
    dst_wave_id: String,
    dst_block_id: Option<String>,
    label_start: usize,
}

/// Visit valid report links in document order, stopping visitation when the
/// visitor returns `false`. Returns whether every scanned link was visited.
pub fn visit_links(markdown: &str, visitor: impl FnMut(ReportLinkRef) -> bool) -> bool {
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS;
    visit_links_with_options(markdown, opts, visitor)
}

fn visit_links_with_options(
    markdown: &str,
    opts: Options,
    mut visitor: impl FnMut(ReportLinkRef) -> bool,
) -> bool {
    for link in scan_links_with_options(markdown, opts).links {
        if !visitor(ReportLinkRef {
            dst_wave_id: link.dst_wave_id,
            dst_block_id: link.dst_block_id,
            label: link.label,
        }) {
            return false;
        }
    }

    true
}

pub fn scan_links(markdown: &str) -> LinkScan {
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS;
    scan_links_with_options(markdown, opts)
}

fn scan_links_with_options(markdown: &str, opts: Options) -> LinkScan {
    let mut plain = String::new();
    let mut links = Vec::new();
    let mut pending = None;
    let mut link_depth = 0usize;
    let mut image_depth = 0usize;

    for event in Parser::new_ext(markdown, opts) {
        match event {
            Event::Start(Tag::Link { dest_url, .. }) => {
                link_depth += 1;
                pending =
                    parse_destination(&dest_url).map(|(dst_wave_id, dst_block_id)| PendingLink {
                        dst_wave_id,
                        dst_block_id,
                        label_start: plain.len(),
                    });
            }
            Event::End(TagEnd::Link) => {
                if let Some(link) = pending.take() {
                    let label_end = plain.len();
                    let label = plain
                        .get(link.label_start..label_end)
                        .expect("link label offsets are character boundaries")
                        .to_string();
                    links.push(ScannedLink {
                        dst_wave_id: link.dst_wave_id,
                        dst_block_id: link.dst_block_id,
                        label,
                        label_start: link.label_start,
                        label_end,
                    });
                }
                link_depth = link_depth.saturating_sub(1);
            }
            Event::Start(Tag::Image { .. }) => image_depth += 1,
            Event::End(TagEnd::Image) => image_depth = image_depth.saturating_sub(1),
            Event::Text(text) | Event::Code(text) if image_depth == 0 || link_depth > 0 => {
                plain.push_str(&text);
            }
            Event::SoftBreak | Event::HardBreak if image_depth == 0 || link_depth > 0 => {
                plain.push('\n');
            }
            Event::End(
                TagEnd::Paragraph
                | TagEnd::Item
                | TagEnd::CodeBlock
                | TagEnd::TableCell
                | TagEnd::TableRow
                | TagEnd::Table
                | TagEnd::FootnoteDefinition,
            )
            | Event::End(TagEnd::Heading(_))
            | Event::End(TagEnd::BlockQuote(_))
            | Event::End(TagEnd::List(_)) => plain.push('\n'),
            Event::Rule
            | Event::Html(_)
            | Event::InlineHtml(_)
            | Event::TaskListMarker(_)
            | Event::FootnoteReference(_) => {}
            _ => {}
        }
    }

    LinkScan { plain, links }
}

fn parse_destination(destination: &str) -> Option<(String, Option<String>)> {
    let path = destination.strip_prefix(WAVE_LINK_PREFIX)?;
    let (wave_id, fragment) = match path.split_once('#') {
        Some((wave_id, fragment)) => (wave_id, Some(fragment)),
        None => (path, None),
    };
    if wave_id.is_empty() || wave_id.contains('/') {
        return None;
    }

    let block_id = fragment.filter(|fragment| is_block_id(fragment));
    Some((wave_id.to_string(), block_id.map(str::to_string)))
}

fn is_block_id(id: &str) -> bool {
    id.len() == 6
        && id.starts_with("b_")
        && id[2..]
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_links(markdown: &str) -> Vec<ReportLinkRef> {
        let mut links = Vec::new();
        assert!(visit_links(markdown, |link| {
            links.push(link);
            true
        }));
        links
    }

    fn collect_links_with_options(markdown: &str, opts: Options) -> Vec<ReportLinkRef> {
        let mut links = Vec::new();
        assert!(visit_links_with_options(markdown, opts, |link| {
            links.push(link);
            true
        }));
        links
    }

    #[test]
    fn fenced_code_block_is_not_a_link() {
        let markdown = "```markdown\n[x](neige://wave/w1)\n```\n";
        assert!(collect_links(markdown).is_empty());
    }

    #[test]
    fn inline_code_span_is_not_a_link() {
        assert!(collect_links("`[x](neige://wave/w1)`").is_empty());
    }

    #[test]
    fn reference_style_link_is_extracted() {
        let links = collect_links("[x][ref]\n\n[ref]: neige://wave/w1#b_1f3a\n");
        assert_eq!(
            links,
            [ReportLinkRef {
                dst_wave_id: "w1".into(),
                dst_block_id: Some("b_1f3a".into()),
                label: "x".into(),
            }]
        );
    }

    #[test]
    fn autolink_is_extracted() {
        let links = collect_links("<neige://wave/w1>");
        assert_eq!(
            links,
            [ReportLinkRef {
                dst_wave_id: "w1".into(),
                dst_block_id: None,
                label: "neige://wave/w1".into(),
            }]
        );
    }

    #[test]
    fn wave_id_is_the_exact_path_segment() {
        let links = collect_links("[x](neige://wave/abc-def)");
        assert_eq!(links[0].dst_wave_id, "abc-def");
        assert_ne!(links[0].dst_wave_id, "abc");
    }

    #[test]
    fn markdown_without_neige_links_is_empty() {
        assert!(collect_links("[web](https://example.com) and [local](/wave/w1)").is_empty());
    }

    #[test]
    fn invalid_fragment_degrades_to_whole_report_link() {
        for fragment in ["b_1F3a", "b_123", "b_12345", "section", "b_1f3a#tail"] {
            let markdown = format!("[x](neige://wave/w1#{fragment})");
            let links = collect_links(&markdown);
            assert_eq!(links[0].dst_block_id, None, "{fragment}");
        }
    }

    #[test]
    fn invalid_wave_paths_are_skipped() {
        for markdown in [
            "[empty](neige://wave/)",
            "[slash](neige://wave/a/b)",
            "[other](neige://card/w1)",
        ] {
            assert!(collect_links(markdown).is_empty(), "{markdown}");
        }
    }

    #[test]
    fn links_preserve_document_order_and_text_labels() {
        let links = collect_links(
            "[first *emphasis*](neige://wave/w1) then [second `code`](neige://wave/w2)",
        );
        assert_eq!(
            links
                .iter()
                .map(|link| (link.dst_wave_id.as_str(), link.label.as_str()))
                .collect::<Vec<_>>(),
            [("w1", "first emphasis"), ("w2", "second code")]
        );
    }

    #[test]
    fn scan_accumulates_all_visible_text_and_byte_spans() {
        let scan =
            scan_links("前文 [web](https://example.com) and [目标](neige://wave/w1#b_1f3a) 后文");

        assert_eq!(scan.plain, "前文 web and 目标 后文\n");
        assert_eq!(scan.links.len(), 1);
        let link = &scan.links[0];
        assert_eq!(link.dst_wave_id, "w1");
        assert_eq!(link.dst_block_id.as_deref(), Some("b_1f3a"));
        assert_eq!(link.label, "目标");
        assert_eq!(
            scan.plain.get(link.label_start..link.label_end),
            Some("目标")
        );
    }

    #[test]
    fn scan_keeps_block_boundaries_separate() {
        let scan = scan_links("first paragraph\n\n[second](neige://wave/w1)\n\nthird");

        assert_eq!(scan.plain, "first paragraph\nsecond\nthird\n");
        assert_eq!(scan.links[0].label, "second");
    }

    #[test]
    fn scan_separates_adjacent_list_items() {
        assert_eq!(scan_links("- first\n- second").plain, "first\nsecond\n\n");
    }

    #[test]
    fn scan_separates_fenced_code_block_from_following_block() {
        assert_eq!(scan_links("```\na\n```\n\nb").plain, "a\n\nb\n");
    }

    #[test]
    fn scan_separates_adjacent_table_cells() {
        assert_eq!(
            scan_links("| first | second |\n| --- | --- |").plain,
            "first\nsecond\n\n"
        );
    }

    #[test]
    fn scan_preserves_table_row_boundaries() {
        assert_eq!(
            scan_links("| head |\n| --- |\n| body |").plain,
            "head\nbody\n\n\n"
        );
    }

    #[test]
    fn scan_separates_adjacent_footnote_definitions() {
        assert_eq!(
            scan_links("[^one]: first\n[^two]: second").plain,
            "first\n\nsecond\n\n"
        );
    }

    #[test]
    fn scan_separates_heading_from_following_block() {
        assert_eq!(scan_links("# heading\nbody").plain, "heading\nbody\n");
    }

    #[test]
    fn scan_preserves_block_quote_boundary() {
        assert_eq!(scan_links("> quoted\n\nafter").plain, "quoted\n\nafter\n");
    }

    #[test]
    fn standalone_image_alt_is_not_plain_text() {
        let scan = scan_links("before ![hidden](image.png) after");

        assert_eq!(scan.plain, "before  after\n");
        assert!(scan.links.is_empty());
    }

    #[test]
    fn image_alt_inside_any_link_is_plain_text() {
        let scan = scan_links("[![visible](image.png)](https://example.com)");

        assert_eq!(scan.plain, "visible\n");
        assert!(scan.links.is_empty());
    }

    #[test]
    fn nested_image_alt_stays_suppressed_until_outer_image_ends() {
        let scan = scan_links("![outer ![inner](inner.png) tail](outer.png) after");

        assert_eq!(scan.plain, " after\n");
    }

    #[test]
    fn code_inside_standalone_image_alt_is_not_plain_text() {
        let scan = scan_links("before ![`hidden`](image.png) after");

        assert_eq!(scan.plain, "before  after\n");
    }

    #[test]
    fn image_alt_inside_neige_link_is_label_and_plain_text() {
        let scan = scan_links("[![visible](image.png)](neige://wave/w1)");

        assert_eq!(scan.plain, "visible\n");
        assert_eq!(scan.links[0].label, "visible");
        assert_eq!(scan.links[0].label_start, 0);
        assert_eq!(scan.links[0].label_end, "visible".len());
    }

    #[test]
    fn empty_image_alt_inside_neige_link_is_a_valid_empty_label() {
        let scan = scan_links("[![](image.png)](neige://wave/w1)");

        assert_eq!(scan.plain, "\n");
        assert_eq!(scan.links[0].label, "");
        assert_eq!(scan.links[0].label_start, 0);
        assert_eq!(scan.links[0].label_end, 0);
    }

    #[test]
    fn visitor_can_stop_before_later_links_are_parsed() {
        let mut visited = Vec::new();
        let completed = visit_links(
            "[first](neige://wave/w1) [second](neige://wave/w2)",
            |link| {
                visited.push(link.dst_wave_id);
                false
            },
        );
        assert!(!completed);
        assert_eq!(visited, ["w1"]);
    }

    #[test]
    fn public_options_extract_footnote_definition_link() {
        assert_eq!(
            collect_links("[^a]: [n](neige://wave/w1)"),
            [ReportLinkRef {
                dst_wave_id: "w1".into(),
                dst_block_id: None,
                label: "n".into(),
            }]
        );
    }

    #[test]
    fn public_options_strip_strikethrough_from_link_label() {
        assert_eq!(
            collect_links("[a ~~b~~ c](neige://wave/w1)")[0].label,
            "a b c"
        );
    }

    #[test]
    fn gfm_options_three_way_delta() {
        struct Case {
            name: &'static str,
            markdown: &'static str,
            default_label: Option<&'static str>,
            gfm_label: Option<&'static str>,
        }

        // Tables and task lists have no observable extraction delta here; these cases document behavior until plain-text semantics land in a later PR.
        let cases = [
            Case {
                name: "table cell",
                markdown: "| link |\n| --- |\n| [x](neige://wave/w1) |",
                default_label: Some("x"),
                gfm_label: Some("x"),
            },
            Case {
                name: "footnote definition",
                markdown: "[^a]: [n](neige://wave/w1)",
                default_label: None,
                gfm_label: Some("n"),
            },
            Case {
                name: "strikethrough",
                markdown: "[a ~~b~~ c](neige://wave/w1)",
                default_label: Some("a ~~b~~ c"),
                gfm_label: Some("a b c"),
            },
            Case {
                name: "footnote-like reference link",
                markdown: "[x][^r]\n\n[^r]: neige://wave/w1\n",
                default_label: Some("x"),
                gfm_label: None,
            },
            Case {
                name: "task list item",
                markdown: "- [ ] [x](neige://wave/w1)",
                default_label: Some("x"),
                gfm_label: Some("x"),
            },
            Case {
                name: "angle autolink",
                markdown: "<neige://wave/w1>",
                default_label: Some("neige://wave/w1"),
                gfm_label: Some("neige://wave/w1"),
            },
            Case {
                name: "bare URI",
                markdown: "neige://wave/w1",
                default_label: None,
                gfm_label: None,
            },
        ];
        let gfm_options = Options::ENABLE_TABLES
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS;

        for case in cases {
            let default_links = collect_links_with_options(case.markdown, Options::empty());
            let gfm_links = collect_links_with_options(case.markdown, gfm_options);
            let expected_links = |label: Option<&str>| {
                label
                    .map(|label| ReportLinkRef {
                        dst_wave_id: "w1".into(),
                        dst_block_id: None,
                        label: label.into(),
                    })
                    .into_iter()
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                default_links,
                expected_links(case.default_label),
                "{} with default options",
                case.name
            );
            assert_eq!(
                gfm_links,
                expected_links(case.gfm_label),
                "{} with GFM options",
                case.name
            );
        }
    }
}
