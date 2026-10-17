//! Conversation naming from the first transcript input, independent of provider and live runtime.
use crate::model::{HarnessInputPresentation, HarnessInputSegment};

/// A persisted candidate, not a history page. `None` at the source means no user input yet;
/// a visible input whose name is `None` still blocks later or queued input.
#[derive(Debug)]
pub struct ConversationTitleSource {
    pub card_id: String,
    pub input: Option<ConversationTitleInput>,
}

/// Persisted source facts and projectable runtime facts from one storage read snapshot.
/// Runtime payloads stay opaque here; the harness owner interprets its typed snapshot.
#[derive(Debug)]
pub struct ConversationTitleContext {
    pub sources: Vec<ConversationTitleSource>,
    pub runtimes: std::collections::HashMap<
        crate::session_projection_repo::CardId,
        crate::session_projection_repo::WorkerSessionProjection,
    >,
}

#[derive(Debug)]
pub enum ConversationTitleInput {
    Structured(Vec<HarnessInputSegment>),
    Legacy(String),
}

/// Same first-user/first-line contract as the conversation transcript UI. Explicit titles
/// bypass this formatter. UTF-16 is the browser's length unit, rather than Rust scalar count.
pub fn conversation_title(text: &str) -> Option<String> {
    let line = text
        .trim_matches(js_whitespace)
        .split('\n')
        .next()?
        .trim_matches(js_whitespace);
    if line.is_empty() {
        return None;
    }
    let units: Vec<u16> = line.encode_utf16().collect();
    Some(if units.len() <= 48 {
        line.to_owned()
    } else {
        format!(
            "{}…",
            String::from_utf16_lossy(&units[..47]).trim_end_matches(js_whitespace)
        )
    })
}

// ECMAScript WhiteSpace + LineTerminator: Rust trim additionally removes NEL and does
// not remove BOM. Keep first-line naming aligned with String.prototype.trim in the UI.
fn js_whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' |
        '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' |
        '\u{3000}' | '\u{feff}')
}

/// Clean the displayed words once, before deciding visibility or deriving a name.
fn user_text(text: &str) -> &str {
    text.strip_prefix("User says:\n")
        .unwrap_or(text)
        .trim_matches(js_whitespace)
}

/// `None` means there is no presentable user input; `Some(None)` is an image-only you.
/// Persisted batches and the projectable pending queue share this exact predicate.
pub fn first_user_title(segments: &[HarnessInputSegment]) -> Option<Option<String>> {
    segments.iter().find_map(|segment| {
        if segment.presentation != HarnessInputPresentation::User {
            return None;
        }
        let text = user_text(&segment.text);
        if text.is_empty() && segment.attachments.is_empty() {
            return None;
        }
        Some(conversation_title(text))
    })
}

impl ConversationTitleInput {
    pub fn title(&self) -> Option<String> {
        self.visible_title().flatten()
    }

    /// Same presentable-you contract for both released transcript encodings. Legacy images
    /// are not decoded by the transcript UI, so only structured attachments count here.
    pub fn visible_title(&self) -> Option<Option<String>> {
        match self {
            Self::Structured(segments) => first_user_title(segments),
            Self::Legacy(params) => {
                let value: serde_json::Value = serde_json::from_str(params).ok()?;
                let content = value.get("item")?.get("content")?.as_array()?;
                let text: String = content
                    .iter()
                    .filter_map(|part| part.get("text")?.as_str())
                    .collect();
                let mut text = text.as_str();
                if text.starts_with("## Track state changes since your last turn")
                    && let Some((_, tail)) = text.split_once("\n\n---\n\n")
                {
                    text = tail;
                }
                let text = user_text(text);
                (!text.is_empty()).then(|| conversation_title(text))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn conversation_title_first_line_utf16_and_legacy_contract() {
        assert_eq!(
            conversation_title(" \r\n  first line\r\nsecond "),
            Some("first line".into())
        );
        assert_eq!(conversation_title(" \n "), None);
        assert_eq!(
            conversation_title("\u{feff}name\u{feff}"),
            Some("name".into())
        );
        assert_eq!(conversation_title("\u{85}name"), Some("\u{85}name".into()));
        assert_eq!(
            conversation_title(&"界".repeat(49)),
            Some(format!("{}…", "界".repeat(47)))
        );
        assert_eq!(
            conversation_title(&"🙂".repeat(25)),
            Some(format!("{}�…", "🙂".repeat(23)))
        );
        let params = json!({"item":{"content":[{"text":"## Track state changes since your last turn\nignored\n\n---\n\nUser says:\nOriginal\n"},{"type":"image"}]}}).to_string();
        assert_eq!(
            ConversationTitleInput::Legacy(params).title(),
            Some("Original".into())
        );
        let segments = serde_json::from_value(json!([
            {"presentation":"system","text":"ignore","attachments":[]},
            {"presentation":"user","text":"User says:\n","attachments":[]},
            {"presentation":"user","text":"first visible segment","attachments":[]}
        ]))
        .unwrap();
        assert_eq!(
            ConversationTitleInput::Structured(segments).title(),
            Some("first visible segment".into())
        );
        let attachment = calm_types::planner_attachment::PlannerAttachment::new(
            &"image-card".into(),
            calm_types::planner_attachment::AttachmentId::parse(
                "12345678-1234-4234-8234-123456789abc.png",
            )
            .unwrap(),
            1,
        );
        let image = HarnessInputSegment {
            presentation: HarnessInputPresentation::User,
            text: String::new(),
            attachments: vec![attachment],
            origin: None,
        };
        assert_eq!(first_user_title(&[image]), Some(None));
    }

    #[test]
    fn conversation_title_visibility_distinguishes_absent_user_from_unnamed_user() {
        for segments in [
            json!([]),
            json!([{"presentation":"system", "text":"Only system", "attachments":[]}]),
            json!([{"presentation":"user", "text":"User says:\n \n", "attachments":[]}]),
        ] {
            let input =
                ConversationTitleInput::Structured(serde_json::from_value(segments).unwrap());
            assert_eq!(input.visible_title(), None);
        }
        // The released legacy decoder ignores image parts and attachment fields.
        let legacy = json!({"item": {
            "content": [{"text":"User says:\n "}, {"type":"image", "url":"ignored.png"}],
            "attachments": ["ignored.png"]
        }})
        .to_string();
        assert_eq!(ConversationTitleInput::Legacy(legacy).visible_title(), None);
    }
}
