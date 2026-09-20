//! Wire vocabulary for captured sources: what `GET /api/tracks/{id}/sources[/{source_id}]` and
//! `calm.source.list` hand out, and the two enums the `report_sources` rows store.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

/// What the body *is*: the Planner's declaration, page-visible. `manual` bodies are the Planner's
/// own and are marked as not kernel-verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "snake_case")]
pub enum SourceProvenance {
    FullText,
    Summary,
    WebPage,
    Manual,
}

impl SourceProvenance {
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

/// Where a body came from. Stored verbatim as the row's `origin` column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceOrigin {
    /// The recorded plugin call the body was read off.
    Plugin {
        plugin_id: String,
        tool: String,
        args_sha256: String,
        /// Canonicalization version behind `args_sha256` (`v1`: compact serde_json text with sorted keys).
        args_canon: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        content_id: Option<String>,
    },
    /// The Planner's own bytes.
    Manual {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        content_id: Option<String>,
    },
}

impl SourceOrigin {
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

/// One anchor of a source: `text` is a byte-exact substring of the body (`body[start..end]`, UTF-8 byte offsets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct SourceQuote {
    pub id: String,
    pub text: String,
    #[ts(type = "number")]
    pub start: usize,
    #[ts(type = "number")]
    pub end: usize,
}

/// A captured source without its body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TrackSourceSummary {
    pub source_id: String,
    pub provenance: SourceProvenance,
    pub origin: SourceOrigin,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub published_at: Option<String>,
    /// `origin.content_id`, surfaced for the panel header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub content_id: Option<String>,
    /// `origin.url` (manual sources only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
    #[ts(type = "number")]
    pub body_bytes: usize,
    pub body_sha256: String,
    /// RFC 3339, UTC.
    pub captured_at: String,
    pub quotes: Vec<SourceQuote>,
}

/// A captured source with its body: the raw text the kernel stored, verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TrackSourceDetail {
    pub source_id: String,
    pub provenance: SourceProvenance,
    pub origin: SourceOrigin,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub published_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub content_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
    #[ts(type = "number")]
    pub body_bytes: usize,
    pub body_sha256: String,
    pub captured_at: String,
    pub quotes: Vec<SourceQuote>,
    pub body: String,
}

impl TrackSourceDetail {
    pub fn from_summary(summary: TrackSourceSummary, body: String) -> Self {
        Self {
            source_id: summary.source_id,
            provenance: summary.provenance,
            origin: summary.origin,
            title: summary.title,
            published_at: summary.published_at,
            content_id: summary.content_id,
            url: summary.url,
            body_bytes: summary.body_bytes,
            body_sha256: summary.body_sha256,
            captured_at: summary.captured_at,
            quotes: summary.quotes,
            body,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TrackSourceList {
    pub sources: Vec<TrackSourceSummary>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_round_trips() {
        for p in [
            SourceProvenance::FullText,
            SourceProvenance::Summary,
            SourceProvenance::WebPage,
            SourceProvenance::Manual,
        ] {
            assert_eq!(SourceProvenance::parse(p.as_str()), Some(p));
            assert_eq!(serde_json::to_value(p).unwrap(), p.as_str());
        }
        assert_eq!(SourceProvenance::parse("fulltext"), None);
    }

    #[test]
    fn origin_json_is_tagged_by_kind() {
        let plugin = SourceOrigin::Plugin {
            plugin_id: "p".into(),
            tool: "t".into(),
            args_sha256: "ab".into(),
            args_canon: "v1".into(),
            content_id: None,
        };
        let value = serde_json::to_value(&plugin).unwrap();
        assert_eq!(value["kind"], "plugin");
        assert!(value.get("content_id").is_none());
        assert_eq!(
            serde_json::from_value::<SourceOrigin>(value).unwrap(),
            plugin
        );
        let manual = SourceOrigin::Manual {
            url: Some("https://x".into()),
            content_id: None,
        };
        let value = serde_json::to_value(&manual).unwrap();
        assert_eq!(value["kind"], "manual");
        assert_eq!(value["url"], "https://x");
    }
}
