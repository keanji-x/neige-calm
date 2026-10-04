//! How Codex spells an MCP tool to the model. The kernel owns raw tool names only; this adapter
//! owns the Codex spelling, which only `calm.source.capture`'s `call.tool` ever receives.
//!
//! Measured on the deployed `codex-cli 0.159.2` (#2003 §3.2, not the stale `external/codex`):
//! - The model sees one Responses `namespace` tool `mcp__<server key>`. Each function in it is the
//!   raw name with every character outside `[A-Za-z0-9_]` replaced by `_`.
//! - `mcp__<server>__<name>` is capped at 128 bytes: under `mcp__calm` a callable name keeps at most
//!   117 bytes. A longer one is cut to 104 bytes and gets `_` plus 12 hex characters of SHA-1.
//! - Two raw names that sanitize to the same string are **both** hash-suffixed.
//! - On `tools/call` Codex sends the **raw** name back, so ordinary calls never depend on this.
//!
//! KNOWN GAP (#2003 K1): a hash-suffixed callable cannot be reduced without copying Codex's
//! internals, so it resolves to no tool and the caller gets the explicit unknown-name refusal.

/// codex-mcp's sanitizing: every char outside `[A-Za-z0-9_]` becomes `_`.
pub(crate) fn codex_sanitized(name: &str) -> String {
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
const CODEX_MCP_PREFIX: &str = "mcp__";
const CODEX_MCP_DELIMITER: &str = "__";

/// The byte cap Codex 0.159.2 applies to `mcp__<server>__<callable>`.
#[cfg(test)]
const CODEX_QUALIFIED_NAME_CAP: usize = 128;

/// The one key every spelling of a registry tool reduces to. A registry name starts with
/// `plugin.` or the kernel prefix, never `mcp__`, so stripping cannot mis-read one.
pub(crate) fn model_tool_key(name: &str) -> String {
    codex_sanitized(strip_codex_qualifier(name))
}

fn strip_codex_qualifier(name: &str) -> &str {
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

    const TRUSTED: &str = "plugin.dev.neige.git-forge_wf.tool";

    #[test]
    fn codex_sanitized_matches_the_responses_api_alphabet() {
        assert_eq!(
            codex_sanitized(TRUSTED),
            "plugin_dev_neige_git_forge_wf_tool"
        );
        assert_eq!(codex_sanitized("a_b9Z"), "a_b9Z");
        assert_eq!(codex_sanitized("é-x"), "__x");
    }

    #[test]
    fn model_tool_key_strips_only_a_delimited_non_empty_server_segment() {
        assert_eq!(model_tool_key("mcp__calm__plugin_a_b"), "plugin_a_b");
        assert_eq!(model_tool_key("mcp__calm__plugin.a-b_c"), "plugin_a_b_c");
        assert_eq!(model_tool_key("mcp__plugin_a_b"), "mcp__plugin_a_b");
        assert_eq!(model_tool_key("mcp____plugin_a_b"), "mcp____plugin_a_b");
        assert_eq!(model_tool_key(TRUSTED), codex_sanitized(TRUSTED));
    }

    /// Codex hashes a callable that is too long or that collides after sanitizing; a kernel tool
    /// must be neither, under every server key the shared CODEX_HOME writes. Then the kernel
    /// tool's callable is exactly its sanitized raw name, and the sanitizing is reversible.
    #[test]
    fn kernel_tool_callables_are_injective_and_unhashed() {
        let registry = crate::mcp_server::build_default_registry();
        let kernel: Vec<String> = registry
            .descriptors()
            .into_iter()
            .map(|descriptor| descriptor.name)
            .filter(|name| !name.starts_with("plugin."))
            .collect();
        assert!(kernel.len() >= 30, "anti-vacuity: {kernel:?}");

        let mut by_callable = std::collections::BTreeMap::new();
        for name in &kernel {
            if let Some(other) = by_callable.insert(codex_sanitized(name), name) {
                panic!("`{name}` and `{other}` share one Codex callable, so Codex hashes both");
            }
        }
        for server in crate::shared_codex_home::EXPECTED_MCP_SERVERS {
            for callable in by_callable.keys() {
                let qualified =
                    format!("{CODEX_MCP_PREFIX}{server}{CODEX_MCP_DELIMITER}{callable}");
                assert!(
                    qualified.len() <= CODEX_QUALIFIED_NAME_CAP,
                    "`{qualified}` is {} bytes; Codex hashes over {CODEX_QUALIFIED_NAME_CAP}",
                    qualified.len()
                );
                assert_eq!(model_tool_key(&qualified), *callable);
            }
        }
    }
}
