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

/// The app-server reports unit variants as strings and connection variants as
/// objects. Connection metadata is intentionally ignored for display.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
enum TerminalErrorInfo {
    ServerOverloaded,
    RateLimitExceeded,
    UsageLimitExceeded,
    ContextWindowExceeded,
    Unauthorized,
    HttpConnectionFailed {},
    ResponseStreamConnectionFailed {},
    ResponseStreamDisconnected {},
}

/// A terminal diagnostic safe to persist and display. Provider messages and
/// details can echo conversation input, so only recognize protocol variants.
pub fn terminal_diagnostic(info: &serde_json::Value) -> Option<(&'static str, &'static str)> {
    use serde::Deserialize;

    Some(match TerminalErrorInfo::deserialize(info).ok()? {
        TerminalErrorInfo::ServerOverloaded => (
            "model_overloaded",
            "The selected model is at capacity. Try again later or select a different model.",
        ),
        TerminalErrorInfo::RateLimitExceeded => (
            "rate_limited",
            "The provider rate limit was reached. Try again later.",
        ),
        TerminalErrorInfo::UsageLimitExceeded => (
            "usage_limit_exceeded",
            "The provider usage limit was reached.",
        ),
        TerminalErrorInfo::ContextWindowExceeded => (
            "context_window_exceeded",
            "The conversation exceeded the model's context window.",
        ),
        TerminalErrorInfo::Unauthorized => (
            "provider_sign_in_required",
            "Codex could not authenticate. Sign in again before retrying.",
        ),
        TerminalErrorInfo::HttpConnectionFailed {}
        | TerminalErrorInfo::ResponseStreamConnectionFailed {}
        | TerminalErrorInfo::ResponseStreamDisconnected {} => (
            "provider_connection_failed",
            "The connection to the provider failed during execution.",
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::terminal_diagnostic;
    use serde_json::json;

    #[test]
    fn terminal_diagnostic_decodes_known_protocol_variants() {
        for (info, code) in [
            (json!("serverOverloaded"), "model_overloaded"),
            (json!("rateLimitExceeded"), "rate_limited"),
            (json!("usageLimitExceeded"), "usage_limit_exceeded"),
            (json!("contextWindowExceeded"), "context_window_exceeded"),
            (json!("unauthorized"), "provider_sign_in_required"),
            (
                json!({"responseStreamDisconnected":{"httpStatusCode":502}}),
                "provider_connection_failed",
            ),
        ] {
            assert_eq!(terminal_diagnostic(&info).map(|(code, _)| code), Some(code));
        }
        assert!(terminal_diagnostic(&json!("unknownFutureVariant")).is_none());
        assert!(terminal_diagnostic(&json!("responseStreamDisconnected")).is_none());
    }
}
