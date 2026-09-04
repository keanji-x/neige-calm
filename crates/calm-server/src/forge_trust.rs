/// #1321 S1 第一轮评审 MINOR-4 — the trusted set is read from the
/// process-global `NEIGE_TRUSTED_FORGE_PLUGINS`, and more than one lib-test
/// module *writes* it (`track_binding::tests::TrustGuard`,
/// `operation::child_track_adapter::tests::trust_inherited_plugin`). A lock
/// private to one of those modules cannot serialize them against each other,
/// and the failure mode is a **vacuous pass**, not a visible error. Every
/// writer in the lib-test binary takes this one lock, so the claim "these
/// tests are mutually exclusive" is true under `cargo test` as well as under
/// nextest's process-per-test.
///
/// Test-only, and deliberately not a guarantee about *readers*: lib tests that
/// only read the ambient value (e.g. `mcp_server::tool_visibility::tests`) do
/// not take it and are still nextest-dependent.
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
