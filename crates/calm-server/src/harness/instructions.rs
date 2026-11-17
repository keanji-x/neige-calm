//! Profile-owned instructions for creation and all recovery paths.
/// One instructions entry point for all managed harness backends and recovery.
pub(crate) async fn profile_instructions(
    repo: &dyn crate::db::Repo,
    plugin: &crate::plugin_host::PluginHost,
    track_id: &str,
    card_id: &str,
    binding: &crate::harness::profile::PlannerBinding,
) -> crate::error::Result<String> {
    use crate::harness::profile::HarnessProfile;
    match binding.profile {
        HarnessProfile::Planner | HarnessProfile::Assistant => {
            crate::operation::planner_harness_start_adapter::planner_instructions(
                repo, plugin, track_id, card_id,
            )
            .await
        }
        HarnessProfile::PlainChat => Ok(String::new()),
    }
}
