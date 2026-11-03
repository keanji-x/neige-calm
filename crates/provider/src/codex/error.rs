//! Native Codex failures, independent of HTTP and persistence.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("codex app-server: {0}")]
    Transport(String),
    #[error("codex refused: {0}")]
    Refused(String),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
}
pub type Result<T> = std::result::Result<T, Error>;

/// A terminal diagnostic safe to persist and display. Provider messages and
/// details can echo conversation input, so only recognize protocol variants.
pub fn terminal_diagnostic(info: &serde_json::Value) -> Option<(&'static str, &'static str)> {
    Some(match info.as_str() {
        Some("serverOverloaded") => (
            "model_overloaded",
            "The selected model is at capacity. Try again later or select a different model.",
        ),
        Some("rateLimitExceeded") => (
            "rate_limited",
            "The provider rate limit was reached. Try again later.",
        ),
        Some("usageLimitExceeded") => (
            "usage_limit_exceeded",
            "The provider usage limit was reached.",
        ),
        Some("contextWindowExceeded") => (
            "context_window_exceeded",
            "The conversation exceeded the model's context window.",
        ),
        Some("unauthorized") => (
            "provider_sign_in_required",
            "Codex could not authenticate. Sign in again before retrying.",
        ),
        None if info.as_object().is_some_and(|object| {
            object.len() == 1
                && [
                    "httpConnectionFailed",
                    "responseStreamConnectionFailed",
                    "responseStreamDisconnected",
                ]
                .iter()
                .any(|code| object.get(*code).is_some_and(serde_json::Value::is_object))
        }) =>
        {
            (
                "provider_connection_failed",
                "The connection to the provider failed during execution.",
            )
        }
        _ => return None,
    })
}
