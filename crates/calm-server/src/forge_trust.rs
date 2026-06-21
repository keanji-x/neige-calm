pub fn trusted_forge_plugin(plugin_id: &str) -> bool {
    let configured = std::env::var("NEIGE_TRUSTED_FORGE_PLUGINS")
        .unwrap_or_else(|_| "dev.neige.git-forge".to_string());
    configured
        .split(',')
        .map(str::trim)
        .any(|trusted| trusted == plugin_id)
}
