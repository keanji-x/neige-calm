//! Permanent Codex refresh failures, recognized only on native failure paths.

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticationFailure {
    Expired,
    Reused,
    Revoked,
    Unknown,
    AccountChanged,
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
        if message.starts_with(
            "your access token could not be refreshed. please log out and sign in again.",
        ) {
            return Some(Self::Unknown);
        }
        if message.starts_with(
            "your access token could not be refreshed because you have \
    since logged out or signed in to another account. please sign in again.",
        ) {
            return Some(Self::AccountChanged);
        }
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

    /// Only the native auth logger's permanent refresh failures are evidence.
    /// stderr cannot attribute a request to a login generation, so callers must
    /// treat this as a reported error, not proof that the current login failed.
    pub fn from_stderr_line(line: &str) -> Option<Self> {
        let plain = strip_log_colors(line);
        let (_, body) = plain.split_once(" ERROR ")?;
        let message = ["codex_core::auth: ", "codex_login::auth::manager: "]
            .iter()
            .find_map(|module| body.strip_prefix(module))?;
        if let Some(message) = message.strip_prefix("Failed to refresh token: ") {
            if let Some(failure) = Self::from_message(message) {
                return Some(failure);
            }
        }
        if !message.starts_with("Token refresh failed: ")
            && !message.starts_with("Failed to refresh token")
        {
            return None;
        }
        let json_start = message.find('{')?;
        let detail: serde_json::Value = serde_json::from_str(&message[json_start..]).ok()?;
        Self::from_code(detail.get("error")?.get("code")?.as_str()?)
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "refresh_token_expired" => Some(Self::Expired),
            "refresh_token_reused" => Some(Self::Reused),
            "refresh_token_invalidated" => Some(Self::Revoked),
            "refresh_token_failed" => Some(Self::Unknown),
            "refresh_token_account_changed" => Some(Self::AccountChanged),
            _ => None,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::Expired => "refresh_token_expired",
            Self::Reused => "refresh_token_reused",
            Self::Revoked => "refresh_token_invalidated",
            Self::Unknown => "refresh_token_failed",
            Self::AccountChanged => "refresh_token_account_changed",
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

// tracing's SGR colors decorate metadata, never change the native logger contract.
fn strip_log_colors(line: &str) -> String {
    let mut plain = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for ch in chars.by_ref() {
                if ch == 'm' {
                    break;
                }
            }
        } else {
            plain.push(ch);
        }
    }
    plain
}

#[cfg(test)]
mod stderr_tests {
    use super::*;
    #[test]
    fn only_native_auth_logger_refresh_records_are_reported() {
        let canonical =
            "Your access token could not be refreshed because your refresh token was already used.";
        assert_eq!(
            AuthenticationFailure::from_stderr_line(&format!(
                "2026-10-07T00:00:00Z ERROR codex_core::auth: Failed to refresh token: {canonical}"
            )),
            Some(AuthenticationFailure::Reused)
        );
        assert_eq!(
            AuthenticationFailure::from_stderr_line(
                "2026-10-07T00:00:00Z ERROR codex_core::auth: Token refresh failed: 401 Unauthorized: {\"error\":{\"code\":\"refresh_token_expired\"}}"
            ),
            Some(AuthenticationFailure::Expired)
        );
        for line in [
            format!("2026-10-07T00:00:00Z ERROR mcp_client::auth: Failed to refresh token: {canonical}"),
            format!("2026-10-07T00:00:00Z ERROR codex_core::tools: {canonical}"),
            "2026-10-07T00:00:00Z ERROR codex_core::auth: Token refresh failed: 503 quota exceeded".into(),
            "2026-10-07T00:00:00Z ERROR codex_core::auth: something else {\"error\":{\"code\":\"refresh_token_reused\"}}".into(),
        ] {assert_eq!(AuthenticationFailure::from_stderr_line(&line),None,"{line}");}
    }
    #[test]
    fn other_native_permanent_refresh_failures_remain_authentication_failures() {
        assert_eq!(
            AuthenticationFailure::from_message(
                "Your access token could not be refreshed. Please log out and sign in again."
            ),
            Some(AuthenticationFailure::Unknown)
        );
        assert_eq!(
            AuthenticationFailure::from_message(
                "Your access token could not be refreshed because you have \
    since logged out or signed in to another account. Please sign in again."
            ),
            Some(AuthenticationFailure::AccountChanged)
        );
        assert_eq!(
            AuthenticationFailure::from_message(
                "unknown model: Your access token could not be refreshed. Please log out and sign in again."
            ),
            None
        );
        assert_eq!(
            AuthenticationFailure::from_message("401 unauthorized"),
            None
        );
    }
}
