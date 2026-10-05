//! How Codex spells an MCP tool to the model. Every name the kernel serves, kernel and plugin tools
//! alike, is minted in `[A-Za-z0-9_]` (#2087 §2, §6), so Codex respells none of them; this module
//! keeps the measured Codex rules the kernel tests check served names against, and the
//! `mcp__<server>__` qualifier `neige_source_capture` strips from a `call.tool`.
//!
//! Measured on the deployed `codex-cli 0.159.2` (#2003 §3.2, not the stale `external/codex`):
//! - The model sees one Responses `namespace` tool `mcp__<server key>`. Each function in it is the
//!   raw name with every character outside `[A-Za-z0-9_]` replaced by `_`.
//! - `mcp__<server>__<name>` is capped at 128 bytes: under `mcp__neige` a callable name keeps at most
//!   116 bytes. A longer one is cut to 104 bytes and gets `_` plus 12 hex characters of SHA-1.
//! - Two raw names that sanitize to the same string are **both** hash-suffixed.
//! - On `tools/call` Codex sends the **raw** name back, so ordinary calls never depend on this.
//!
//! KNOWN GAP (#2003 K1): a hash-suffixed callable cannot be reduced without copying Codex's
//! internals, so it resolves to no tool and the caller gets the explicit unknown-name refusal.

/// codex-mcp's sanitizing: every char outside `[A-Za-z0-9_]` becomes `_`. Served names are its
/// fixed points; the kernel tests assert that, so no production path calls it.
pub fn codex_sanitized(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The model reads `mcp__<server>__<sanitized tool>`.
pub const CODEX_MCP_PREFIX: &str = "mcp__";
pub const CODEX_MCP_DELIMITER: &str = "__";

/// The byte cap Codex 0.159.2 applies to `mcp__<server>__<callable>`.
pub const CODEX_QUALIFIED_NAME_CAP: usize = 128;

/// The served name inside `mcp__<server>__<name>`; any other string is returned as it is. A
/// registry name starts with `plugin_` or the kernel prefix, never `mcp__`, so stripping cannot
/// mis-read one.
pub fn strip_codex_qualifier(name: &str) -> &str {
    match name
        .strip_prefix(CODEX_MCP_PREFIX)
        .and_then(|rest| rest.split_once(CODEX_MCP_DELIMITER))
    {
        Some((server, tool)) if !server.is_empty() => tool,
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_sanitized_matches_the_responses_api_alphabet() {
        assert_eq!(
            codex_sanitized("plugin_dev.x-y_wf.tool"),
            "plugin_dev_x_y_wf_tool"
        );
        assert_eq!(codex_sanitized("a_b9Z"), "a_b9Z");
        assert_eq!(codex_sanitized("é-x"), "__x");
    }

    #[test]
    fn strip_codex_qualifier_strips_only_a_delimited_non_empty_server_segment() {
        assert_eq!(
            strip_codex_qualifier("mcp__neige__plugin_a_b"),
            "plugin_a_b"
        );
        assert_eq!(
            strip_codex_qualifier("mcp__neige__plugin_a-b_c"),
            "plugin_a-b_c"
        );
        assert_eq!(strip_codex_qualifier("mcp__plugin_a_b"), "mcp__plugin_a_b");
        assert_eq!(
            strip_codex_qualifier("mcp____plugin_a_b"),
            "mcp____plugin_a_b"
        );
        assert_eq!(
            strip_codex_qualifier("plugin_gitforge_gh_pr_checks"),
            "plugin_gitforge_gh_pr_checks"
        );
    }
}
