//! Neutral inputs shared by agent providers.

/// A single text or local-image input, interpreted by the consuming provider.
#[derive(Debug, Clone)]
pub enum InputItem {
    Text { text: String },
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
