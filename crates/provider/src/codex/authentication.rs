//! A Codex sign-in failure, read from the app-server's structured turn error.

use serde_json::Value;

/// App-server v2 raises `TurnError.codexErrorInfo = "unauthorized"` only for a refresh token Codex
/// can no longer use (`CodexErr::RefreshTokenFailed`). A transient refresh failure is `other` and a
/// model 401 is `httpConnectionFailed`, so neither is one (probed on 0.159.2, #2512).
const SIGN_IN_FAILURE: &str = "unauthorized";

/// Whether a `TurnError` (the `error` notification's `error`, or `turn/completed`'s `turn.error`)
/// says the sign-in must be renewed. Its `message` is prose and decides nothing.
pub fn is_sign_in_failure(turn_error: &Value) -> bool {
    turn_error.get("codexErrorInfo").and_then(Value::as_str) == Some(SIGN_IN_FAILURE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_codex_error_info_unauthorized_is_a_sign_in_failure() {
        assert!(is_sign_in_failure(
            &json!({"message":"Sign in again.","codexErrorInfo":"unauthorized"})
        ));
        for error in [
            json!({"message":"Your access token could not be refreshed because your refresh token has expired.","codexErrorInfo":"other"}),
            json!({"message":"unexpected status 401","codexErrorInfo":{"httpConnectionFailed":{"httpStatusCode":401}}}),
            json!({"message":"Your access token could not be refreshed because your refresh token was revoked."}),
            json!({"message":"x","codexErrorInfo":null}),
        ] {
            assert!(!is_sign_in_failure(&error), "{error}");
        }
    }
}
