//! Compatibility re-export of the theme DTO from the truth model.

pub use calm_truth::model::RequestTheme;

#[cfg(test)]
mod tests {
    use super::*;

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
        let raw = r#"{"fg":[0,0,0],"bg":[1,1,1],"extra":"junk"}"#;
        let err = serde_json::from_str::<RequestTheme>(raw)
            .expect_err("deny_unknown_fields must reject extras");
        // A bare `is_err()` is satisfied by any deserialize failure, so assert the unknown-field message.
        let message = err.to_string();
        assert!(
            message.contains("unknown field") && message.contains("extra"),
            "rejection must be serde's unknown-field error naming `extra`; got: {message}"
        );
    }
}
