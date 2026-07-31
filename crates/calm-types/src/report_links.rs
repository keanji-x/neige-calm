//! Extraction of typed `neige://` links from report markdown.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

const WAVE_LINK_PREFIX: &str = "neige://wave/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportLinkRef {
    pub dst_wave_id: String,
    pub dst_block_id: Option<String>,
    pub label: String,
}

struct PendingLink {
    dst_wave_id: String,
    dst_block_id: Option<String>,
    label: String,
}

/// Visit valid report links in document order, stopping parsing when the
/// visitor returns `false`. Returns whether the entire markdown was scanned.
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
    let mut pending = None;

    for event in Parser::new_ext(markdown, opts) {
        match event {
            Event::Start(Tag::Link { dest_url, .. }) => {
                pending =
                    parse_destination(&dest_url).map(|(dst_wave_id, dst_block_id)| PendingLink {
                        dst_wave_id,
                        dst_block_id,
                        label: String::new(),
                    });
            }
            Event::End(TagEnd::Link) => {
                if let Some(link) = pending.take()
                    && !visitor(ReportLinkRef {
                        dst_wave_id: link.dst_wave_id,
                        dst_block_id: link.dst_block_id,
                        label: link.label,
                    })
                {
                    return false;
                }
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some(link) = &mut pending {
                    link.label.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(link) = &mut pending {
                    link.label.push('\n');
                }
            }
            _ => {}
        }
    }

    true
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
    fn gfm_options_change_only_footnote_definition_link_extraction() {
        struct Case {
            name: &'static str,
            markdown: &'static str,
            default_label: Option<&'static str>,
            gfm_label: Option<&'static str>,
        }

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
                markdown: "~~[x](neige://wave/w1)~~",
                default_label: Some("x"),
                gfm_label: Some("x"),
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
