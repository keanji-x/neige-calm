//! #1669 S1 — captured source texts (`report_sources` rows) and what the
//! kernel vouches for about them.
//!
//! A source belongs to one track. Its `body` and metadata are immutable
//! and it cannot be deleted; `quotes` only grow (each an exact byte
//! substring of `body`, I2). Sources follow their track — FK cascade on
//! delete, a verbatim copy on fork (I3). A `provenance != manual` body is
//! the text the kernel's plugin proxy returned to the Planner for one
//! recorded call (I1); `manual` bodies are the Planner's own bytes and say
//! so on the page.
//!
//! Modules: `store` (rows), `warnings` (unresolved `neige://source/` links
//! in a report write, for the receipt).

pub mod store;
pub mod warnings;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::CalmError;

pub use store::{Detail, NewSource, SourceRow};
pub use warnings::SourceLinkWarning;

/// `body` upper bound; a larger body is refused, never truncated.
pub const MAX_BODY_BYTES: usize = 256 * 1024;
pub const MAX_TITLE_BYTES: usize = 512;
pub const MAX_CONTENT_ID_BYTES: usize = 256;
pub const MAX_URL_BYTES: usize = 2 * 1024;
pub const MAX_PUBLISHED_AT_BYTES: usize = 64;
pub const MAX_QUOTE_BYTES: usize = 2 * 1024;
/// Cumulative per source, appends included.
pub const MAX_QUOTES_PER_SOURCE: usize = 32;
/// Per-track quota, checked inside the capture transaction.
pub const MAX_SOURCES_PER_TRACK: i64 = 128;
pub const MAX_BODY_BYTES_PER_TRACK: i64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = SourceProvenance)]
pub enum Provenance {
    FullText,
    Summary,
    WebPage,
    Manual,
}

impl Provenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FullText => "full_text",
            Self::Summary => "summary",
            Self::WebPage => "web_page",
            Self::Manual => "manual",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "full_text" => Self::FullText,
            "summary" => Self::Summary,
            "web_page" => Self::WebPage,
            "manual" => Self::Manual,
            _ => return None,
        })
    }
}

/// Where a body came from. Stored as the `origin` JSON column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[schema(as = SourceOrigin)]
pub enum Origin {
    /// The recorded plugin call the body was read off.
    Plugin {
        plugin_id: String,
        tool: String,
        args_sha256: String,
        /// Canonicalization version behind `args_sha256` (`v1`: compact
        /// serde_json text with sorted keys).
        args_canon: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_id: Option<String>,
    },
    /// The Planner's own bytes.
    Manual {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_id: Option<String>,
    },
}

impl Origin {
    pub fn content_id(&self) -> Option<&str> {
        match self {
            Self::Plugin { content_id, .. } | Self::Manual { content_id, .. } => {
                content_id.as_deref()
            }
        }
    }

    pub fn url(&self) -> Option<&str> {
        match self {
            Self::Manual { url, .. } => url.as_deref(),
            Self::Plugin { .. } => None,
        }
    }
}

/// One anchor: `text` is `body[start..end]`, byte offsets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Quote {
    pub id: String,
    pub text: String,
    pub start: usize,
    pub end: usize,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    crate::plugin_results::sha256_hex(bytes)
}

/// `captured_at` on the wire: RFC 3339, second precision, UTC.
pub fn captured_at_text(captured_at_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(captured_at_ms)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| captured_at_ms.to_string())
}

/// Per requested quote text, in request order, the anchor id it maps to.
pub type QuoteMapping = Vec<(String, String)>;

/// Resolve requested quote texts against `body` on top of the anchors the
/// source already has. Returns the full anchor list after the append and,
/// per requested text in request order, the id it maps to (an existing
/// anchor with the same text keeps its id; duplicates within the request
/// collapse to one). Every refusal is a `BadRequest` naming the quote.
pub fn append_quotes(
    body: &str,
    existing: &[Quote],
    requested: &[String],
) -> Result<(Vec<Quote>, QuoteMapping), CalmError> {
    let mut quotes = existing.to_vec();
    let mut mapping = Vec::with_capacity(requested.len());
    for (index, text) in requested.iter().enumerate() {
        if text.is_empty() {
            return Err(CalmError::BadRequest(format!("quotes[{index}] is empty")));
        }
        if text.len() > MAX_QUOTE_BYTES {
            return Err(CalmError::BadRequest(format!(
                "quotes[{index}] exceeds {MAX_QUOTE_BYTES} bytes"
            )));
        }
        if let Some(found) = quotes.iter().find(|quote| quote.text == *text) {
            mapping.push((text.clone(), found.id.clone()));
            continue;
        }
        let Some(start) = body.find(text.as_str()) else {
            return Err(CalmError::BadRequest(format!(
                "quotes[{index}] is not a byte-exact substring of the body: {}",
                excerpt(text)
            )));
        };
        if quotes.len() >= MAX_QUOTES_PER_SOURCE {
            return Err(CalmError::BadRequest(format!(
                "a source carries at most {MAX_QUOTES_PER_SOURCE} quotes"
            )));
        }
        let id = format!("q{}", quotes.len() + 1);
        quotes.push(Quote {
            id: id.clone(),
            text: text.clone(),
            start,
            end: start + text.len(),
        });
        mapping.push((text.clone(), id));
    }
    Ok((quotes, mapping))
}

fn excerpt(text: &str) -> String {
    const MAX: usize = 48;
    if text.len() <= MAX {
        return format!("{text:?}");
    }
    let mut end = MAX;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{:?}…", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn quotes_take_the_first_byte_exact_occurrence() {
        let body = "前言。加息，九成。再说：加息，九成。";
        let quote = "加息，九成";
        let (quotes, mapping) = append_quotes(body, &[], &texts(&[quote])).unwrap();
        assert_eq!(quotes.len(), 1);
        assert_eq!(quotes[0].id, "q1");
        assert_eq!(quotes[0].start, "前言。".len());
        assert_eq!(quotes[0].end, "前言。".len() + quote.len());
        assert_eq!(&body[quotes[0].start..quotes[0].end], quote);
        assert_eq!(mapping, vec![(quote.to_string(), "q1".to_string())]);
    }

    #[test]
    fn quotes_must_be_byte_exact_substrings() {
        let body = "**bold** and plain";
        let err = append_quotes(body, &[], &texts(&["bold and"])).unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(m) if m.contains("quotes[0]")));
        let err = append_quotes(body, &[], &texts(&[""])).unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(m) if m.contains("empty")));
        let big = "x".repeat(MAX_QUOTE_BYTES + 1);
        let err = append_quotes(&big, &[], std::slice::from_ref(&big)).unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(m) if m.contains("exceeds")));
    }

    #[test]
    fn duplicates_collapse_and_existing_texts_keep_their_ids() {
        let body = "one two three";
        let existing = vec![Quote {
            id: "q1".into(),
            text: "two".into(),
            start: 4,
            end: 7,
        }];
        let (quotes, mapping) =
            append_quotes(body, &existing, &texts(&["three", "two", "three"])).unwrap();
        assert_eq!(quotes.len(), 2);
        assert_eq!(quotes[1].id, "q2");
        assert_eq!(quotes[1].text, "three");
        assert_eq!(
            mapping,
            vec![
                ("three".to_string(), "q2".to_string()),
                ("two".to_string(), "q1".to_string()),
                ("three".to_string(), "q2".to_string()),
            ]
        );
    }

    #[test]
    fn cumulative_cap_counts_existing_anchors() {
        let body: String = (0..40).map(|i| format!("w{i} ")).collect();
        let existing: Vec<Quote> = (0..MAX_QUOTES_PER_SOURCE)
            .map(|i| {
                let text = format!("w{i} ");
                let start = body.find(&text).unwrap();
                Quote {
                    id: format!("q{}", i + 1),
                    text: text.clone(),
                    start,
                    end: start + text.len(),
                }
            })
            .collect();
        // An existing text is still fine at the cap …
        assert!(append_quotes(&body, &existing, &texts(&["w3 "])).is_ok());
        // … a new one is not.
        let err = append_quotes(&body, &existing, &texts(&["w39 "])).unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(m) if m.contains("at most")));
    }

    #[test]
    fn provenance_round_trips() {
        for p in [
            Provenance::FullText,
            Provenance::Summary,
            Provenance::WebPage,
            Provenance::Manual,
        ] {
            assert_eq!(Provenance::parse(p.as_str()), Some(p));
            assert_eq!(serde_json::to_value(p).unwrap(), p.as_str());
        }
        assert_eq!(Provenance::parse("fulltext"), None);
    }

    #[test]
    fn origin_json_is_tagged_by_kind() {
        let plugin = Origin::Plugin {
            plugin_id: "p".into(),
            tool: "t".into(),
            args_sha256: "ab".into(),
            args_canon: "v1".into(),
            content_id: None,
        };
        let value = serde_json::to_value(&plugin).unwrap();
        assert_eq!(value["kind"], "plugin");
        assert!(value.get("content_id").is_none());
        assert_eq!(serde_json::from_value::<Origin>(value).unwrap(), plugin);
        let manual = Origin::Manual {
            url: Some("https://x".into()),
            content_id: None,
        };
        let value = serde_json::to_value(&manual).unwrap();
        assert_eq!(value["kind"], "manual");
        assert_eq!(value["url"], "https://x");
    }
}
