//! Kernel-facing proxy resolution and explicit application settings.
pub use provider::codex::shared::{effective_proxy_env_from, resolved_proxy_env_pairs};

/// Explicit application settings take precedence after project evaluation.
/// Ambient proxy defaults remain subject to that project's overrides and unsets.
pub(crate) async fn configured_proxy_env_pairs(
    repo: &dyn crate::db::RepoRead,
) -> crate::error::Result<Vec<(&'static str, &'static str, String)>> {
    let settings = crate::routes::settings::load_settings(repo).await?;
    Ok(resolved_proxy_env_pairs(
        settings.http_proxy.as_deref(),
        settings.https_proxy.as_deref(),
        |_| None,
    ))
}
