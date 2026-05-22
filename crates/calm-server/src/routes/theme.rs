//! Shared `RequestTheme` type for routes that forward the host browser's
//! current theme RGB onto the `calm-session-daemon` argv (#177).
//!
//! Originally lived in `routes::codex_cards` since the codex-card create
//! endpoint was the first call site. Lifted here so the wave-create
//! endpoint (`routes::waves`) can reuse the same wire shape — both routes
//! ultimately drive the same [`crate::routes::terminal::SpawnDaemonOpts`]
//! and need to render `(r, g, b)` tuples as `"r,g,b"` for the daemon CLI.
//!
//! Re-exported from `routes::codex_cards` for back-compat with the
//! original location (PR #193 shipped it there); new call sites should
//! import from here directly.

use serde::Deserialize;
use utoipa::ToSchema;

/// Wire shape of `NewCodexCardBody.theme` / `NewWave.theme`. Matches the
/// `calm_session::TerminalTheme` value type one-for-one — duplicated
/// here so the route can keep its own `ToSchema` derive (the
/// `calm_session` crate is utoipa-free).
#[derive(Deserialize, Debug, Clone, Copy, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestTheme {
    pub fg: (u8, u8, u8),
    pub bg: (u8, u8, u8),
}

impl RequestTheme {
    /// Render `(r, g, b)` as the comma-decimal form `r,g,b` the
    /// daemon CLI expects on `--terminal-fg` / `--terminal-bg`.
    pub fn fg_arg(&self) -> String {
        let (r, g, b) = self.fg;
        format!("{r},{g},{b}")
    }
    pub fn bg_arg(&self) -> String {
        let (r, g, b) = self.bg;
        format!("{r},{g},{b}")
    }
}

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
        let result: Result<RequestTheme, _> = serde_json::from_str(raw);
        assert!(
            result.is_err(),
            "deny_unknown_fields must reject extras; got: {result:?}"
        );
    }
}
