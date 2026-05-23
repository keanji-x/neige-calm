//! Tiny in-house glob matcher shared by the plugin host.
//!
//! Hoisted here in #198 followup C: prior to this commit the same three-line
//! matcher lived twice — once in `events.rs` (for `neige.event.subscribe`
//! filter globs) and once in `perms.rs` (for `permissions.tools` allow-list
//! checks on iframe-initiated `tools/call`). The two copies were byte-identical
//! by deliberate design, and a future tweak to either grammar should land in
//! exactly one place.
//!
//! Supported pattern grammar:
//!
//!   * `"*"` — matches every name (firehose grant).
//!   * `"<prefix>.*"` — matches anything whose name starts with `"<prefix>."`,
//!     i.e. a one-segment-or-more dot-anchored suffix wildcard. The dot anchor
//!     is load-bearing: `"card.*"` matches `"card.added"` but NOT `"cardx.added"`.
//!     `"<prefix>.*"` also matches multi-segment names like `"card.x.y"` — there
//!     is no enforcement that exactly one segment follows.
//!   * Anything else — literal equality.
//!
//! We intentionally do NOT pull a glob crate: filter input arrives from plugin
//! processes the kernel does not audit, and a regex-style DoS would be exactly
//! the kind of foot-gun the host slice is meant to avoid.

/// Match `name` against `pattern` using the kernel's narrow glob grammar.
///
/// See module docs for the supported shapes. `pub(super)` keeps the helper
/// scoped to `plugin_host` — no consumer outside the host needs this.
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

// ===========================================================================
// Tests
// ===========================================================================

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
        // Unrelated prefix.
        assert!(!glob_matches("card.*", "wave.added"));
    }

    #[test]
    fn tool_name_prefix_wildcard() {
        // Mirrors the perms.rs use case: `neige.overlay.*` against tool names.
        assert!(glob_matches("neige.overlay.*", "neige.overlay.set"));
        assert!(glob_matches("neige.overlay.*", "neige.overlay.delete"));
        // Dot anchor: `neige.overlayx` must not slip through `neige.overlay.*`.
        assert!(!glob_matches("neige.overlay.*", "neige.overlayx"));
        // Different namespace.
        assert!(!glob_matches("neige.overlay.*", "neige.card.update"));
    }

    #[test]
    fn unknown_pattern_falls_through_to_literal() {
        // No support for mid-string globs — falls through to literal match,
        // so `"card.*.added"` only matches itself verbatim.
        assert!(glob_matches("card.*.added", "card.*.added"));
        assert!(!glob_matches("card.*.added", "card.x.added"));
    }
}
