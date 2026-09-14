//! #1669 — extraction of `neige://source/<source_id>[#q<n>]` links from
//! report markdown.
//!
//! A sibling of [`crate::report_links`], not an extension of it: that
//! scanner's records mean track/block references and feed task-dependency
//! projection, frozen task context and backlinks. Source citations must
//! never reach any of those (#1669 I5), so they get their own prefix, their
//! own scan, and no shared record type. Same parser, same options, same
//! rule that code spans and fenced code are not links.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

pub const SOURCE_LINK_PREFIX: &str = "neige://source/";

/// One resolved `neige://source/…` link, in document order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLinkRef {
    pub source_id: String,
    /// `q<n>` when the destination carried a well-formed anchor.
    pub quote_id: Option<String>,
    /// The destination exactly as pulldown-cmark decoded it.
    pub destination: String,
}

/// `src_` + 8 lowercase hex digits.
pub fn is_source_id(id: &str) -> bool {
    id.len() == 12
        && id.starts_with("src_")
        && id[4..]
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// `q` + a positive decimal without leading zeros.
pub fn is_quote_id(id: &str) -> bool {
    let Some(digits) = id.strip_prefix('q') else {
        return false;
    };
    !digits.is_empty()
        && !digits.starts_with('0')
        && digits.bytes().all(|byte| byte.is_ascii_digit())
}

/// `neige://source/<source_id>[#<quote_id>]` → `(source_id, quote_id)`.
/// A malformed source id is not a source link at all; a malformed
/// anchor keeps the source link and drops the anchor (mirrors
/// `report_links::parse_destination`, whose bad fragment degrades to a
/// whole-report link).
pub fn parse_source_destination(destination: &str) -> Option<(String, Option<String>)> {
    let path = destination.strip_prefix(SOURCE_LINK_PREFIX)?;
    let (source_id, fragment) = match path.split_once('#') {
        Some((source_id, fragment)) => (source_id, Some(fragment)),
        None => (path, None),
    };
    if !is_source_id(source_id) {
        return None;
    }
    let quote_id = fragment.filter(|fragment| is_quote_id(fragment));
    Some((source_id.to_string(), quote_id.map(str::to_string)))
}

pub fn format_source_destination(source_id: &str, quote_id: Option<&str>) -> String {
    let mut destination = format!("{SOURCE_LINK_PREFIX}{source_id}");
    if let Some(quote_id) = quote_id {
        destination.push('#');
        destination.push_str(quote_id);
    }
    destination
}

/// Every `neige://source/…` link in `markdown`, in document order. Inline,
/// reference-style and autolinks all count; links inside code spans and
/// fenced code do not (they are text there).
pub fn scan(markdown: &str) -> Vec<SourceLinkRef> {
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS;
    let mut links = Vec::new();
    let mut pending: Option<SourceLinkRef> = None;
    for event in Parser::new_ext(markdown, opts) {
        match event {
            Event::Start(Tag::Link { dest_url, .. }) => {
                pending = parse_source_destination(&dest_url).map(|(source_id, quote_id)| {
                    SourceLinkRef {
                        source_id,
                        quote_id,
                        destination: dest_url.to_string(),
                    }
                });
            }
            Event::End(TagEnd::Link) => {
                if let Some(link) = pending.take() {
                    links.push(link);
                }
            }
            _ => {}
        }
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(source_id: &str, quote_id: Option<&str>) -> SourceLinkRef {
        SourceLinkRef {
            source_id: source_id.into(),
            quote_id: quote_id.map(str::to_string),
            destination: format_source_destination(source_id, quote_id),
        }
    }

    #[test]
    fn inline_link_with_anchor_is_extracted() {
        let links = scan("see ([Mikko](neige://source/src_2c9e0a1b#q1)) here\n");
        assert_eq!(links, [link("src_2c9e0a1b", Some("q1"))]);
    }

    #[test]
    fn inline_link_without_anchor_is_extracted() {
        let links = scan("[x](neige://source/src_2c9e0a1b)");
        assert_eq!(links, [link("src_2c9e0a1b", None)]);
    }

    #[test]
    fn reference_style_link_is_extracted() {
        let links = scan("[x][ref]\n\n[ref]: neige://source/src_2c9e0a1b#q12\n");
        assert_eq!(links, [link("src_2c9e0a1b", Some("q12"))]);
    }

    #[test]
    fn autolink_is_extracted() {
        let links = scan("<neige://source/src_2c9e0a1b#q3>");
        assert_eq!(links, [link("src_2c9e0a1b", Some("q3"))]);
    }

    #[test]
    fn code_span_and_fence_are_not_links() {
        assert!(scan("`[x](neige://source/src_2c9e0a1b)`").is_empty());
        assert!(scan("```md\n[x](neige://source/src_2c9e0a1b#q1)\n```\n").is_empty());
    }

    #[test]
    fn malformed_source_ids_are_not_source_links() {
        assert!(scan("[x](neige://source/src_dead)").is_empty());
        assert!(scan("[x](neige://source/src_2C9E0A1B)").is_empty());
        assert!(scan("[x](neige://source/2c9e0a1b)").is_empty());
        assert!(scan("[x](neige://source/)").is_empty());
        let track_link = crate::report_links::format_track_destination("w1", Some("b_1f3a"));
        assert!(scan(&format!("[x]({track_link})")).is_empty());
    }

    #[test]
    fn malformed_anchor_keeps_the_source_link_and_drops_the_anchor() {
        let links =
            scan("[x](neige://source/src_2c9e0a1b#q0) [y](neige://source/src_2c9e0a1b#b_1f3a)");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].source_id, "src_2c9e0a1b");
        assert_eq!(links[0].quote_id, None);
        assert_eq!(links[0].destination, "neige://source/src_2c9e0a1b#q0");
        assert_eq!(links[1].quote_id, None);
    }

    /// The two scanners are disjoint: neither sees the other's scheme.
    #[test]
    fn track_links_are_invisible_here_and_source_links_are_invisible_to_track_links() {
        let track_link = crate::report_links::format_track_destination("w1", Some("b_1f3a"));
        let markdown = format!("[a]({track_link}) [b](neige://source/src_2c9e0a1b#q1)");
        assert_eq!(scan(&markdown), [link("src_2c9e0a1b", Some("q1"))]);
        let mut track_links = Vec::new();
        assert!(crate::report_links::visit_links(&markdown, |l| {
            track_links.push(l);
            true
        }));
        assert_eq!(track_links.len(), 1);
        assert_eq!(track_links[0].dst_track_id, "w1");
    }

    #[test]
    fn document_order_and_duplicates_are_preserved() {
        let links = scan(
            "[a](neige://source/src_2c9e0a1b#q2)\n\n[b](neige://source/src_2c9e0a1b#q2)\n\n[c](neige://source/src_0000ffff)",
        );
        assert_eq!(
            links,
            [
                link("src_2c9e0a1b", Some("q2")),
                link("src_2c9e0a1b", Some("q2")),
                link("src_0000ffff", None),
            ]
        );
    }

    #[test]
    fn id_predicates() {
        assert!(is_source_id("src_0123abcd"));
        assert!(!is_source_id("src_0123abc"));
        assert!(!is_source_id("src_0123ABCD"));
        assert!(!is_source_id("b_1f3a"));
        assert!(is_quote_id("q1"));
        assert!(is_quote_id("q32"));
        assert!(!is_quote_id("q"));
        assert!(!is_quote_id("q0"));
        assert!(!is_quote_id("q01"));
        assert!(!is_quote_id("Q1"));
    }
}
