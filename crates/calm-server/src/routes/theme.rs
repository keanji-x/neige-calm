//! Compatibility path for the theme DTO moved with the truth model in #679 PR2.

pub use calm_truth::model::RequestTheme;

#[cfg(test)]
mod tests {
    use super::*;

    /// JSON round-trip + arg rendering. Pins the wire shape so a future
    /// rename of `fg` / `bg` (or a structural change like flattening to
    /// `[r, g, b]` arrays) fails loudly in a unit test instead of silently
    /// breaking the daemon-argv contract.
    #[test]
    fn json_roundtrip_renders_to_daemon_args() {
        let raw = r#"{"fg":[216,219,226],"bg":[15,20,24]}"#;
        let theme: RequestTheme = serde_json::from_str(raw).expect("parse json");
        assert_eq!(theme.fg, (216, 219, 226));
        assert_eq!(theme.bg, (15, 20, 24));
        assert_eq!(theme.fg_arg(), "216,219,226");
        assert_eq!(theme.bg_arg(), "15,20,24");
    }

    #[test]
    fn rejects_unknown_fields() {
        // Defensive: the body uses `deny_unknown_fields` so a stale
        // caller that sends `{ "fg": ..., "bg": ..., "extra": ... }`
        // is rejected at the deserialize step.
        let raw = r#"{"fg":[0,0,0],"bg":[1,1,1],"extra":"junk"}"#;
        let err = serde_json::from_str::<RequestTheme>(raw)
            .expect_err("deny_unknown_fields must reject extras");
        // Assert WHY it failed, not merely that it failed. `is_err()` is
        // satisfied by any deserialize failure of this literal — drop
        // `deny_unknown_fields` and rename a required field and the parse
        // still errors ("missing field"), so a bare `is_err()` would keep
        // reporting "unknown-field policy intact" after the policy is gone.
        let message = err.to_string();
        assert!(
            message.contains("unknown field") && message.contains("extra"),
            "rejection must be serde's unknown-field error naming `extra`; got: {message}"
        );
    }
}
