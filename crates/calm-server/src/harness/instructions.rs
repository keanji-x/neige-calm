//! Profile-owned instructions for creation and all recovery paths.
use crate::planner_card::{
    ASSISTANT_SYSTEM_PROMPT_TEMPLATE, LAUNCHPAD_ASSISTANT_SYSTEM_PROMPT_TEMPLATE,
    render_system_prompt,
};
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
        HarnessProfile::Planner => {
            crate::operation::planner_harness_start_adapter::planner_instructions(
                repo, plugin, track_id, card_id,
            )
            .await
        }
        HarnessProfile::PlainChat => Ok(String::new()),
        HarnessProfile::Assistant => {
            let template = if crate::routes::today::is_launchpad_track(repo, track_id).await? {
                LAUNCHPAD_ASSISTANT_SYSTEM_PROMPT_TEMPLATE
            } else {
                ASSISTANT_SYSTEM_PROMPT_TEMPLATE
            };
            let template = crate::harness::backend::PlannerBackend::metadata(&binding.provider)
                .discovery
                .render_assistant(template);
            Ok(render_system_prompt(&template, track_id))
        }
    }
}
