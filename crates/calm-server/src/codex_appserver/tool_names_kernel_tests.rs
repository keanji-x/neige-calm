use provider::codex::tool_names::{
    CODEX_MCP_DELIMITER, CODEX_MCP_PREFIX, CODEX_QUALIFIED_NAME_CAP, codex_sanitized,
    model_tool_key,
};

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
            let qualified = format!("{CODEX_MCP_PREFIX}{server}{CODEX_MCP_DELIMITER}{callable}");
            assert!(
                qualified.len() <= CODEX_QUALIFIED_NAME_CAP,
                "`{qualified}` is {} bytes; Codex hashes over {CODEX_QUALIFIED_NAME_CAP}",
                qualified.len()
            );
            assert_eq!(model_tool_key(&qualified), *callable);
        }
    }
}
