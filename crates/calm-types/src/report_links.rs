//! Extraction of typed `neige://` links from report markdown.

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

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

/// Return valid report links in document order.
pub fn extract_links(markdown: &str) -> Vec<ReportLinkRef> {
    let mut links = Vec::new();
    visit_links(markdown, |link| {
        links.push(link);
        true
    });
    links
}

/// Visit valid report links in document order, stopping parsing when the
/// visitor returns `false`. Returns whether the entire markdown was scanned.
pub fn visit_links(markdown: &str, mut visitor: impl FnMut(ReportLinkRef) -> bool) -> bool {
    let mut pending = None;

    for event in Parser::new(markdown) {
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

    #[test]
    fn fenced_code_block_is_not_a_link() {
        let markdown = "```markdown\n[x](neige://wave/w1)\n```\n";
        assert!(extract_links(markdown).is_empty());
    }

    #[test]
    fn inline_code_span_is_not_a_link() {
        assert!(extract_links("`[x](neige://wave/w1)`").is_empty());
    }

    #[test]
    fn reference_style_link_is_extracted() {
        let links = extract_links("[x][ref]\n\n[ref]: neige://wave/w1#b_1f3a\n");
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
        let links = extract_links("<neige://wave/w1>");
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
        let links = extract_links("[x](neige://wave/abc-def)");
        assert_eq!(links[0].dst_wave_id, "abc-def");
        assert_ne!(links[0].dst_wave_id, "abc");
    }

    #[test]
    fn markdown_without_neige_links_is_empty() {
        assert!(extract_links("[web](https://example.com) and [local](/wave/w1)").is_empty());
    }

    #[test]
    fn invalid_fragment_degrades_to_whole_report_link() {
        for fragment in ["b_1F3a", "b_123", "b_12345", "section", "b_1f3a#tail"] {
            let markdown = format!("[x](neige://wave/w1#{fragment})");
            let links = extract_links(&markdown);
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
            assert!(extract_links(markdown).is_empty(), "{markdown}");
        }
    }

    #[test]
    fn links_preserve_document_order_and_text_labels() {
        let links = extract_links(
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
}
