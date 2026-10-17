//! Complete operator fixtures for catalogs that exercise all running compiled components.
#![cfg(unix)]
use super::opencode_history as history;
use crate::support::mcp::CardBoot;
use calm_server::{builtin_plugins, state::WriteContext};
pub use history::HistoryHost;

pub async fn running(boot: &CardBoot) -> HistoryHost {
    let directory = boot._tmp.path().join("history");
    std::fs::create_dir_all(&directory).unwrap();
    let history = history::boot(
        boot.repo.clone(),
        boot.events.clone(),
        WriteContext::new(boot.card_role_cache.clone(), boot.track_area_cache.clone()),
        &directory,
        true,
    )
    .await;
    history.host.reconcile_builtins().await.unwrap();
    for component in builtin_plugins::catalog() {
        history.host.enable(&component.manifest().id).await.unwrap();
    }
    assert!(boot.plugin_host.set(history.host.clone()).is_ok());
    history
}
