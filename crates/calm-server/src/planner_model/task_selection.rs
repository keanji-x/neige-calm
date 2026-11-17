//! Task declaration selection; provider defaults belong to the task contract.
use super::{CardModelSelection, acquisition::CatalogAccess};
use crate::error::{CalmError, Result};
use crate::model::TaskKind;
use crate::session_projection_repo::AgentProvider;
use provider::TurnModelSelection;

fn task_provider(
    kind: TaskKind,
    declared: Option<AgentProvider>,
    planner: &AgentProvider,
) -> Result<AgentProvider> {
    let provider = match kind {
        TaskKind::Codex => {
            if declared
                .as_ref()
                .is_some_and(|p| *p != AgentProvider::Codex)
            {
                return Err(CalmError::BadRequest(
                    "task.provider conflicts with kind codex".into(),
                ));
            }
            AgentProvider::Codex
        }
        TaskKind::Agent => declared.unwrap_or_else(|| match planner {
            AgentProvider::Claude => AgentProvider::Codex,
            supported @ (AgentProvider::Codex
            | AgentProvider::Antigravity
            | AgentProvider::OpenCode
            | AgentProvider::Grok) => supported.clone(),
        }),
        _ => {
            return Err(CalmError::BadRequest(
                "unsupported_worker_provider: task kind is not an agent worker".into(),
            ));
        }
    };
    if provider == AgentProvider::Claude {
        return Err(CalmError::BadRequest(
            "unsupported_worker_provider: claude".into(),
        ));
    }
    Ok(provider)
}

/// Omitted values follow provider defaults on the fresh worker session. Explicit values are
/// judged against the live catalog, including an effort supplied without an explicit model.
#[allow(dead_code)] // Consumed by the step 3 worker adapter.
pub(crate) async fn resolve_task_selection(
    access: &CatalogAccess<'_>,
    kind: TaskKind,
    declared: Option<AgentProvider>,
    model: Option<String>,
    effort: Option<String>,
    planner: &AgentProvider,
    cwd: &str,
) -> Result<(AgentProvider, TurnModelSelection, CardModelSelection)> {
    let provider = task_provider(kind, declared, planner)?;
    for (field, value) in [("model", &model), ("reasoning_effort", &effort)] {
        if value.as_ref().is_some_and(|v| v.trim().is_empty()) {
            return Err(CalmError::BadRequest(format!(
                "task.{field} must not be empty"
            )));
        }
    }
    if model.is_some() || effort.is_some() {
        let catalog = access.read(&provider, None, Some(cwd)).await?;
        validate_task_selection(&catalog, model.as_deref(), effort.as_deref()).await?;
    }
    let card = CardModelSelection {
        model_ever_set: model.is_some(),
        reasoning_effort_ever_set: effort.is_some(),
        model: model.clone(),
        reasoning_effort: effort.clone(),
    };
    Ok((provider, TurnModelSelection { model, effort }, card))
}

async fn validate_task_selection(
    catalog: &super::catalog::ModelsResponse,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<()> {
    let effective = model.or(catalog.default.model.as_deref());
    if effective.is_none() || catalog.source == super::catalog::ModelSource::Unavailable {
        return Err(CalmError::BadRequest(
            "task.model: catalog default unavailable; cannot validate selection".into(),
        ));
    }
    super::selection_advice::catalog_advice(
        super::selection_advice::CatalogSource::Discovered(catalog),
        effective,
        effort,
    )
    .await
    .require_child_selection(effective, effort)
    .map_err(|e| CalmError::BadRequest(format!("task selection: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_worker_provider_inheritance_and_alias_admission() {
        for planner in [
            AgentProvider::Codex,
            AgentProvider::Antigravity,
            AgentProvider::OpenCode,
            AgentProvider::Grok,
        ] {
            assert_eq!(
                task_provider(TaskKind::Agent, None, &planner).unwrap(),
                planner
            );
            assert_eq!(
                task_provider(TaskKind::Codex, None, &planner).unwrap(),
                AgentProvider::Codex
            );
        }
        assert_eq!(
            task_provider(TaskKind::Agent, None, &AgentProvider::Claude).unwrap(),
            AgentProvider::Codex
        );
        assert!(matches!(
            task_provider(
                TaskKind::Agent,
                Some(AgentProvider::Claude),
                &AgentProvider::Codex
            ),
            Err(CalmError::BadRequest(_))
        ));
        assert!(
            task_provider(
                TaskKind::Codex,
                Some(AgentProvider::Grok),
                &AgentProvider::Codex
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn task_worker_resolver_preserves_native_defaults_and_rejects_empty_values() {
        let repo = std::sync::Arc::new(
            crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
                .await
                .unwrap(),
        );
        let wiring = crate::harness::wiring::PlannerWiring::unconfigured_for_test(repo.clone());
        let codex = crate::shared_codex_appserver::SharedCodexAppServer::new_stub(repo);
        let availability = crate::agent_providers::ProviderAvailabilityCache::default();
        let access = CatalogAccess::new(&availability, &wiring.claude, &wiring.acp, &codex);
        let (provider, turn, card) = resolve_task_selection(
            &access,
            TaskKind::Agent,
            None,
            None,
            None,
            &AgentProvider::Grok,
            "/tmp",
        )
        .await
        .unwrap();
        assert_eq!(provider, AgentProvider::Grok);
        assert_eq!(turn, TurnModelSelection::inherit());
        assert_eq!(card, CardModelSelection::default());
        assert!(matches!(
            resolve_task_selection(
                &access,
                TaskKind::Agent,
                None,
                Some(" ".into()),
                None,
                &AgentProvider::Codex,
                "/tmp"
            )
            .await,
            Err(CalmError::BadRequest(_))
        ));
    }

    #[tokio::test]
    async fn task_worker_model_and_effort_are_catalog_validated() {
        let catalog = serde_json::from_value(serde_json::json!({
            "models": [{"id":"preset", "model":"known", "resolved_model":null,"display_name":"Known","description":"", "is_default":true,"supported_reasoning_efforts":[{"reasoning_effort":"high","description":null}],"default_reasoning_effort":"high"}],
            "default":{"model":"known","reasoning_effort":"high","supported_reasoning_efforts":null},
            "default_source":"acp_session","source":"live","fetched_at_ms":null
        })).unwrap();
        assert!(
            validate_task_selection(&catalog, Some("known"), Some("high"))
                .await
                .is_ok()
        );
        assert!(
            validate_task_selection(&catalog, Some("missing"), None)
                .await
                .is_err()
        );
        assert!(
            validate_task_selection(&catalog, Some("known"), Some("invalid"))
                .await
                .is_err()
        );
        assert!(
            validate_task_selection(&catalog, None, Some("invalid"))
                .await
                .is_err()
        );
        let mut unknown_default = catalog;
        unknown_default.default.model = None;
        assert!(
            validate_task_selection(&unknown_default, None, Some("high"))
                .await
                .is_err()
        );
    }
}
