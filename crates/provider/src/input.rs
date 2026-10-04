//! Inputs shared by agent providers; preserve the existing transcript and wire shapes.

use serde::Serialize;

/// A single `turn/start` / `turn/steer` input item.
/// codex's `UserInput` is `camelCase` while this enum is `rename_all = "lowercase"`: a variant added without its own `rename` would serialize as `"localimage"`, which codex rejects with no signal on our side.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum InputItem {
    /// `{"type":"text","text":"…"}`.
    Text { text: String },
    /// `{"type":"localImage","path":"/abs/path"}` — codex reads the file itself (same mount namespace). `detail` is deliberately not sent.
    /// A read or decode failure on codex's side is silent (placeholder text, no error), so a successful `turn/start` is no evidence the image was seen.
    #[serde(rename = "localImage")]
    LocalImage { path: String },
}

impl InputItem {
    /// Convenience constructor for the text variant.
    pub fn text(s: impl Into<String>) -> Self {
        InputItem::Text { text: s.into() }
    }

    /// Convenience constructor for the local-image variant.
    pub fn local_image(path: impl Into<String>) -> Self {
        InputItem::LocalImage { path: path.into() }
    }
}

/// What a `turn/start` frame must say about the model. `None` means the key is not put on the frame at all, leaving whatever the thread already carries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnModelSelection {
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl TurnModelSelection {
    /// Send neither key. Correct only where we have never put a sticky value on the thread.
    pub fn inherit() -> Self {
        Self::default()
    }
}
