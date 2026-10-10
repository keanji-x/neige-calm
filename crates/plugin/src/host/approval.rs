//! approval-delegation responsibilities of the plugin host.
use super::*;

impl<E: ErrorFactory> PluginHost<E> {
    /// The minted names of every delegated tool (`crate::approval`) of the installed and enabled
    /// plugins: registry manifests whose row says `enabled`. Enabled, not running: the grant
    /// follows the owner's decision, not boot autospawn or crash backoff, and a call to a plugin
    /// that is not running is still refused by routing. `claimed` holds the names another source
    /// serves (a compiled built-in's natives), which are never granted. A row read failure is an
    /// error, never a guess.
    pub async fn approval_delegated_tools(
        &self,
        claimed: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, E> {
        let enabled: BTreeSet<String> = self
            .plugin_list_db
            .plugins_list_all()
            .await?
            .into_iter()
            .filter(|row| row.enabled)
            .map(|row| row.id)
            .collect();
        let manifests: Vec<Manifest> = self
            .registry
            .list()
            .into_iter()
            .filter(|manifest| enabled.contains(&manifest.id))
            .collect();
        Ok(crate::approval::delegated_tool_names(&manifests, claimed))
    }
}
