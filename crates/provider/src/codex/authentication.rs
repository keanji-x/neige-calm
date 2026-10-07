//! Permanent Codex refresh failures, recognized only on native failure paths.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthenticationFailure {
    Expired,
    Reused,
    Revoked,
}

impl AuthenticationFailure {
    pub fn from_message(message: &str) -> Option<Self> {
        let message = message.trim_start().to_ascii_lowercase();
        let message = [
            "turn/start failed: ",
            "turn/steer failed: ",
            "account/read failed: ",
        ]
        .iter()
        .find_map(|prefix| message.strip_prefix(prefix))
        .unwrap_or(&message);
        let message =
            message.strip_prefix("your access token could not be refreshed because your ")?;
        if message.starts_with("refresh token was already used") {
            Some(Self::Reused)
        } else if message.starts_with("refresh token has expired") {
            Some(Self::Expired)
        } else if message.starts_with("refresh token was revoked") {
            Some(Self::Revoked)
        } else {
            None
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::Expired => "refresh_token_expired",
            Self::Reused => "refresh_token_reused",
            Self::Revoked => "refresh_token_invalidated",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permanent_refresh_failures_are_narrow_and_sanitized() {
        for (message, expected) in [
            (
                "Your access token could not be refreshed because your refresh token was already used. private-fixture-detail",
                AuthenticationFailure::Reused,
            ),
            (
                "Your access token could not be refreshed because your refresh token has expired.",
                AuthenticationFailure::Expired,
            ),
            (
                "Your access token could not be refreshed because your refresh token was revoked.",
                AuthenticationFailure::Revoked,
            ),
        ] {
            let failure = AuthenticationFailure::from_message(message).unwrap();
            assert_eq!(failure, expected);
            assert!(!failure.code().contains("private-fixture-detail"));
        }
        for unrelated in [
            "quota exceeded",
            "unknown model",
            "Unknown model: Your access token could not be refreshed because your refresh token was already used.",
            "401 MCP authorization failed",
            "too many tokens",
            "token budget expired",
        ] {
            assert_eq!(
                AuthenticationFailure::from_message(unrelated),
                None,
                "{unrelated}"
            );
        }
    }
}
