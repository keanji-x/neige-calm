//! The Codex backend's model resolution: the card's payload plus, where it cannot answer alone,
//! codex's own config and model catalog.

use std::time::Duration;

use serde_json::Value;

use crate::error::CalmError;
use crate::harness::issuance::{IssuanceRefusal, SelectionSource};
use crate::planner_model::{
    CardModelSelection, InstallationDefaults, TurnModelSelection,
    effective_model_for_catalog_lookup, resolve_turn_selection,
};
use crate::shared_codex_appserver::SharedCodexAppServer;

/// Budget for the two codex reads a model resolution may need, together. Generous because
/// elapsing costs a refused turn the person then has to retry.
const MODEL_RESOLUTION_BUDGET: Duration = Duration::from_secs(15);

/// Classify a codex call that failed: codex ANSWERING with a refusal becomes `refused(log)`,
/// every other failure is retryable. The Codex-only reads on the issuance path (`config/read`,
/// `model/list`) go through this; `turn/start` is classified by its backend.
fn classify_codex_failure(
    e: &CalmError,
    log: String,
    refused: impl FnOnce(String) -> IssuanceRefusal,
) -> IssuanceRefusal {
    if matches!(e, CalmError::CodexRefused(_)) {
        refused(log)
    } else {
        IssuanceRefusal::retryable(log)
    }
}

/// Work out what this turn must tell codex about the model, from the card's payload plus —
/// only where the payload cannot answer alone — codex's own config and catalog. `Err` when the
/// answer cannot be established; the turn is not sent under an unknown model.
pub(super) async fn resolve(
    daemon: &SharedCodexAppServer,
    source: &impl SelectionSource,
    payload: &Value,
) -> std::result::Result<TurnModelSelection, IssuanceRefusal> {
    // A payload we cannot read does not start reading itself. Somebody has to
    // write a selection over it, and `PUT /planner/model` does exactly that.
    let card = CardModelSelection::from_payload(payload).map_err(|e| {
        IssuanceRefusal::needs_a_choice(
            e.to_string(),
            "This conversation's saved model selection cannot be read. Pick a model to replace it."
                .into(),
        )
    })?;
    if !card.needs_installation_defaults() {
        // The overwhelmingly common path: the payload is the whole answer.
        return resolve_turn_selection(&card, None, None).map_err(unresolved);
    }

    let deadline = tokio::time::Instant::now() + MODEL_RESOLUTION_BUDGET;
    let cwd = source.installation_cwd().await?;
    // Codex not answering and codex answering "no model" are different facts; only the first
    // is worth waiting out, so the read's failure returns here rather than degrading to `None`.
    let config = daemon
        .config_read(Some(cwd.as_str()), deadline)
        .await
        .map_err(|e| {
            // The sentence is DERIVED: this branch is entered by a disjunction (model, effort, or both
            // follow the default) and a fixed string is right for at most one of them.
            let needed = card.defaults_needed_for();
            let reader = match (needed.subject(), needed.choice_to_make()) {
                (Some(subject), Some(choice)) => format!(
                    "codex will not report this conversation's configuration, so the default \
                     {subject} cannot be resolved and your message has not been sent. Pick \
                     {choice} explicitly to send it."
                ),
                // Unreachable: this read only happens when something is
                // needed. Fail closed with no advice rather than invent some.
                _ => "codex will not report this conversation's configuration, so your message \
                      has not been sent."
                    .to_string(),
            };
            classify_codex_failure(
                &e,
                format!("config/read failed while resolving this conversation's defaults: {e}"),
                |log| IssuanceRefusal::needs_a_choice(log, reader),
            )
        })?;
    let defaults = Some(InstallationDefaults {
        model: config.model,
        reasoning_effort: config.model_reasoning_effort,
    });

    // Only the effort's last fallback wants the catalog, and only when the
    // config did not already answer it.
    let catalog_effort = if card.needs_catalog()
        && defaults
            .as_ref()
            .is_none_or(|d| d.reasoning_effort.is_none())
    {
        catalog_default_effort(daemon, &card, defaults.as_ref(), deadline).await?
    } else {
        None
    };

    resolve_turn_selection(&card, defaults.as_ref(), catalog_effort.as_deref()).map_err(unresolved)
}

/// Only reached once codex has answered, so the answer did not name a model — a person must act.
fn unresolved(e: crate::planner_model::UnresolvedSelection) -> IssuanceRefusal {
    IssuanceRefusal::needs_a_choice(e.log_reason().to_string(), e.reason().to_string())
}

/// Codex's own preset effort for the model that will actually run. `Ok(None)` means the
/// catalog genuinely has no answer; a read that failed is an `Err`.
async fn catalog_default_effort(
    daemon: &SharedCodexAppServer,
    card: &CardModelSelection,
    defaults: Option<&InstallationDefaults>,
    deadline: tokio::time::Instant,
) -> std::result::Result<Option<String>, IssuanceRefusal> {
    let Some(slug) = effective_model_for_catalog_lookup(card, defaults) else {
        // Nothing names a model, so there is no catalog entry to look up. A
        // real absence, not a failed read.
        return Ok(None);
    };
    match daemon.model_list(deadline).await {
        Ok(models) => Ok(models
            .into_iter()
            .find(|m| m.model == slug)
            .map(|m| m.default_reasoning_effort)),
        // Only the effort can want the catalog, so a fixed sentence is honest here.
        Err(e) => Err(classify_codex_failure(
            &e,
            format!("model/list failed while resolving this conversation's default effort: {e}"),
            |log| {
                IssuanceRefusal::needs_a_choice(
                    log,
                    "codex will not list its models, so the default reasoning effort cannot be \
                     resolved and your message has not been sent. Pick a reasoning effort \
                     explicitly to send it."
                        .to_string(),
                )
            },
        )),
    }
}
