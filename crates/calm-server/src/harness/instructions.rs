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
        HarnessProfile::TaskWorker => {
            let template = crate::planner_card::SeededCardRole::WorkerCodex.prompt_template();
            let template = crate::harness::backend::PlannerBackend::metadata(&binding.provider)
                .discovery
                .render_planner(template);
            Ok(render_system_prompt(&template, track_id))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::profile::{HarnessProfile, PlannerBinding};
    use crate::session_projection_repo::AgentProvider;

    #[tokio::test]
    async fn task_worker_instructions_reuse_worker_prompt() {
        let repo = std::sync::Arc::new(
            crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
                .await
                .unwrap(),
        );
        let wiring = crate::harness::wiring::PlannerWiring::unconfigured_for_test(repo.clone());
        let binding = PlannerBinding {
            profile: HarnessProfile::TaskWorker,
            provider: AgentProvider::Codex,
        };
        let prompt = profile_instructions(
            repo.as_ref(),
            &wiring.plugin,
            "worker-track",
            "worker-card",
            &binding,
        )
        .await
        .unwrap();
        assert_eq!(
            prompt,
            crate::planner_card::render_worker_prompt_for_e2e("worker-track")
        );
        for provider in [
            AgentProvider::Antigravity,
            AgentProvider::OpenCode,
            AgentProvider::Grok,
        ] {
            let binding = PlannerBinding {
                profile: HarnessProfile::TaskWorker,
                provider,
            };
            let prompt = profile_instructions(
                repo.as_ref(),
                &wiring.plugin,
                "worker-track",
                "worker-card",
                &binding,
            )
            .await
            .unwrap();
            assert!(prompt.contains("worker-track"));
            assert!(!prompt.contains("{track_id}"));
        }
    }
}
