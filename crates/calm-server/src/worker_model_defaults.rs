//! Worker-owned selection at publication and launch. Never reads defaults on recovery.
use crate::error::{CalmError, Result};
use calm_types::worker_model::{
    WorkerModelChoice, WorkerModelDefaults, WorkerModelProvider, WorkerModelSelection,
};
use provider::TurnModelSelection;

pub(crate) async fn validate_patch(
    patch: &mut std::collections::BTreeMap<String, Option<String>>,
    s: &crate::state::RouteState,
    cs: &crate::state::CodexShellState,
) -> Result<()> {
    for provider in [WorkerModelProvider::Codex, WorkerModelProvider::Claude] {
        let key = provider.setting_key();
        let Some(Some(value)) = patch.get_mut(key) else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        let invalid = |reason| CalmError::InvalidField {
            field: format!("settings.{key}"),
            reason,
        };
        let pair = WorkerModelDefaults::parse(value).map_err(invalid)?;
        let agent = match provider {
            WorkerModelProvider::Codex => crate::session_projection_repo::AgentProvider::Codex,
            WorkerModelProvider::Claude => crate::session_projection_repo::AgentProvider::Claude,
        };
        let advice = crate::planner_model::acquisition::CatalogAccess::new(
            &s.provider_availability,
            &s.claude_planner,
            &s.acp_planner,
            &cs.shared_codex_appserver,
        )
        .canonical_mint(&agent, pair.model.as_deref(), pair.effort.as_deref())
        .await?;
        // A directory is not an allowlist. In particular Claude's Planner advice drops
        // unknown-model efforts, but workers must pass the unchanged pair to the provider.
        if !advice.unknown_model && !advice.catalog_unavailable {
            advice
                .require_create_selection(pair.model.as_deref(), pair.effort.as_deref())
                .map_err(|e| invalid(e.to_string()))?;
        }
        *value = serde_json::to_string(&pair)?;
    }
    Ok(())
}

pub(crate) async fn snapshot_tx(
    tx: &mut crate::operation::Tx<'_>,
    provider: WorkerModelProvider,
) -> Result<WorkerModelSelection> {
    let key = provider.setting_key();
    let selection = match crate::db::sqlite::settings_get_tx(tx, key).await? {
        None => WorkerModelChoice::Inherit,
        Some(value) => {
            WorkerModelChoice::Explicit(WorkerModelDefaults::parse(&value).map_err(|e| {
                CalmError::Internal(format!(
                    "invalid stored {key}: {e}; clear or replace the setting"
                ))
            })?)
        }
    };
    Ok(WorkerModelSelection {
        provider,
        selection,
    })
}

pub(crate) fn selection(snapshot: &WorkerModelSelection) -> TurnModelSelection {
    match &snapshot.selection {
        WorkerModelChoice::Inherit => TurnModelSelection::inherit(),
        WorkerModelChoice::Explicit(pair) => TurnModelSelection {
            model: pair.model.clone(),
            effort: pair.effort.clone(),
        },
    }
}

pub(crate) fn from_payload(
    payload: &serde_json::Value,
    provider: WorkerModelProvider,
) -> Result<Option<TurnModelSelection>> {
    WorkerModelSelection::from_payload(payload)
        .map_err(|e| {
            CalmError::Conflict(format!(
                "invalid worker_model_selection: {e}; stored worker selection cannot be recovered"
            ))
        })?
        .map(|snapshot| {
            if snapshot.provider != provider {
                return Err(CalmError::Conflict(
                    "worker_model_selection provider does not match the bound worker backend"
                        .into(),
                ));
            }
            Ok(selection(&snapshot))
        })
        .transpose()
}

pub(crate) fn claude_flags(selection: &TurnModelSelection) -> String {
    provider::claude::selection_args(selection)
        .into_iter()
        .map(|arg| format!(" {}", crate::shell_command::sh_single_quote(&arg)))
        .collect()
}

pub(crate) fn warn_legacy_selection(card_id: &str, provider: WorkerModelProvider) {
    tracing::warn!(
        card_id,
        ?provider,
        "legacy worker has no model snapshot; pre-upgrade model and effort cannot be guaranteed"
    );
}

#[cfg(test)]
pub(crate) mod tests;
