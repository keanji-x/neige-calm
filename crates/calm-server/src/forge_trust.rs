/// Every lib-test writer of the process-global `NEIGE_TRUSTED_FORGE_PLUGINS` takes this one
/// lock; a lock private to one module cannot serialize them, and the failure is a vacuous pass.
#[cfg(test)]
pub(crate) fn trusted_forge_plugins_env_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

pub fn trusted_forge_plugin(plugin_id: &str) -> bool {
    let configured = std::env::var("NEIGE_TRUSTED_FORGE_PLUGINS")
        .unwrap_or_else(|_| "dev.neige.git-forge".to_string());
    configured
        .split(',')
        .map(str::trim)
        .any(|trusted| trusted == plugin_id)
}
