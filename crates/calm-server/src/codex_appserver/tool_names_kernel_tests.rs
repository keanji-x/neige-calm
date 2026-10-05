use provider::codex::tool_names::{
    CODEX_MCP_DELIMITER, CODEX_MCP_PREFIX, CODEX_QUALIFIED_NAME_CAP, codex_sanitized,
    strip_codex_qualifier,
};

/// Codex hashes a callable that is too long or that collides after sanitizing; a kernel tool
/// must be neither, under every server key the shared CODEX_HOME writes. A kernel name is in
/// Codex's alphabet, so its callable is the raw name itself: nothing respells it.
#[test]
fn kernel_tool_callables_are_injective_and_unhashed() {
    let registry = crate::mcp_server::build_default_registry();
    let kernel: Vec<String> = registry
        .descriptors()
        .into_iter()
        .map(|descriptor| descriptor.name)
        .filter(|name| !name.starts_with(crate::plugin_results::PLUGIN_TOOL_PREFIX))
        .collect();
    assert!(kernel.len() >= 30, "anti-vacuity: {kernel:?}");

    let mut by_callable = std::collections::BTreeMap::new();
    for name in &kernel {
        assert_eq!(&codex_sanitized(name), name, "Codex would respell `{name}`");
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
            assert_eq!(strip_codex_qualifier(&qualified), callable.as_str());
        }
    }
}

/// #2087 §2: `mcp__<server>__<name>` stays within Codex's 128 bytes for every kernel tool and
/// every native plugin tool the repository ships (the built-ins and `plugins/*/manifest.json`),
/// so Codex never cuts and hashes one. Connector tools keep their upstream names and are exempt.
#[test]
fn served_tool_names_fit_the_codex_cap() {
    use crate::plugin_host::Manifest;
    use crate::plugin_host::manifest::ConnectorKind;

    let mut manifests: std::collections::BTreeMap<String, Manifest> =
        crate::builtin_plugins::catalog()
            .iter()
            .map(|builtin| (builtin.manifest().id.clone(), builtin.manifest().clone()))
            .collect();
    let plugins = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    for entry in std::fs::read_dir(&plugins).expect("read plugins/") {
        let path = entry.expect("plugins/ entry").path().join("manifest.json");
        if let Ok(text) = std::fs::read_to_string(&path) {
            let manifest = Manifest::parse(&text)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            manifests.insert(manifest.id.clone(), manifest);
        }
    }
    let native: Vec<String> = manifests
        .values()
        .filter(|manifest| matches!(manifest.kind, ConnectorKind::App | ConnectorKind::Builtin))
        .flat_map(|manifest| {
            manifest
                .exposes_tools
                .iter()
                .map(|tool| crate::plugin_results::registry_name(&manifest.id, &tool.name))
        })
        .collect();
    assert!(native.len() >= 20, "anti-vacuity: {native:?}");
    let kernel = crate::mcp_server::build_default_registry()
        .descriptors()
        .into_iter()
        .map(|descriptor| descriptor.name)
        .filter(|name| !name.starts_with(crate::plugin_results::PLUGIN_TOOL_PREFIX));
    for name in kernel.chain(native) {
        assert_eq!(codex_sanitized(&name), name, "Codex would respell `{name}`");
        for server in crate::shared_codex_home::EXPECTED_MCP_SERVERS {
            let qualified = format!("{CODEX_MCP_PREFIX}{server}{CODEX_MCP_DELIMITER}{name}");
            assert!(
                qualified.len() <= CODEX_QUALIFIED_NAME_CAP,
                "`{qualified}` is {} bytes; Codex hashes over {CODEX_QUALIFIED_NAME_CAP}",
                qualified.len()
            );
        }
    }
}
