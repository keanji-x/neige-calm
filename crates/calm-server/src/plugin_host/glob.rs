//! Tiny in-house glob matcher shared by the plugin host: `"*"`, a dot-anchored `"<prefix>.*"` (any number of following segments), or literal equality.
//! No glob crate on purpose: filter input arrives from plugin processes the kernel does not audit.

pub(super) fn glob_matches(pattern: &str, name: &str) -> bool {
    if pattern == "*" || pattern == name {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix(".*") {
        // "card.*" matches "card.added" but not "cardx.added" — enforce the dot.
        let with_dot = format!("{prefix}.");
        return name.starts_with(&with_dot);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_equality() {
        assert!(glob_matches("card.added", "card.added"));
        assert!(!glob_matches("card.added", "card.updated"));
    }

    #[test]
    fn full_wildcard_matches_anything() {
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("*", "neige.overlay.set"));
        assert!(glob_matches("*", ""));
    }

    #[test]
    fn prefix_wildcard_is_dot_anchored() {
        assert!(glob_matches("card.*", "card.added"));
        assert!(glob_matches("card.*", "card.x.y"));
        // Dot anchor enforced: "cardx.added" must NOT match "card.*".
        assert!(!glob_matches("card.*", "cardx.added"));
        assert!(!glob_matches("card.*", "track.added"));
    }

    #[test]
    fn tool_name_prefix_wildcard() {
        assert!(glob_matches("neige.overlay.*", "neige.overlay.set"));
        assert!(glob_matches("neige.overlay.*", "neige.overlay.delete"));
        // Dot anchor: `neige.overlayx` must not slip through `neige.overlay.*`.
        assert!(!glob_matches("neige.overlay.*", "neige.overlayx"));
        assert!(!glob_matches("neige.overlay.*", "neige.card.update"));
    }

    #[test]
    fn unknown_pattern_falls_through_to_literal() {
        // No mid-string globs: falls through to literal match.
        assert!(glob_matches("card.*.added", "card.*.added"));
        assert!(!glob_matches("card.*.added", "card.x.added"));
    }
}
