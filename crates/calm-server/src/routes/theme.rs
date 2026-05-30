//! Shared `RequestTheme` type for routes that forward the host browser's
//! current theme RGB onto terminal startup (#177).
//!
//! Originally lived in `routes::codex_cards` since the codex-card create
//! endpoint was the first call site. Lifted here so the wave-create
//! endpoint (`routes::waves`) can reuse the same wire shape — both routes
//! render `(r, g, b)` tuples as `"r,g,b"` for terminal startup.
//!
//! After the #177 root-cause refactor the value lands on the
//! `terminals.theme_fg / .theme_bg` columns (NOT NULL via migration
//! 0013) inside the row-creation transaction. The spawn helper
//! (`routes::terminal::spawn_terminal_for`) reads the row at every
//! spawn — there is no separate `SpawnDaemonOpts` carry between
//! transaction commit and terminal startup anymore.

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

    /// Dark-theme sentinel for non-browser-stamped paths (dispatcher
    /// workers, direct repo `wave_create` calls in tests) where there
    /// is no user-visible theme to forward and the code still needs to
    /// produce *a* concrete `RequestTheme`. Mirrors `DARK_THEME_RGB`
    /// in `web/src/api/themeRgb.ts` so dispatcher-spawned codex
    /// workers paint against the same defaults a dark-mode browser
    /// would have stamped. Never returned to the user-facing wave-
    /// create / codex-card routes — those take theme from the request
    /// body and 422 a missing field.
    pub fn default_dark() -> Self {
        Self {
            fg: (216, 219, 226),
            bg: (15, 20, 24),
        }
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
