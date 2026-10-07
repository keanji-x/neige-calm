//! `PUT /api/cards/{id}/planner/model` — records which model a card's conversation
//! runs with. Both keys are required and `null` is a value; an unknown slug is
//! reported, not refused; an unsupported effort is moved to the model's default. One advice for
//! both providers (#1822 6′): only the catalog's source differs, Codex's `model/list` asked now or
//! the Claude CLI's list the availability check cached. A Claude write needs Claude ready (400
//! with the reason otherwise); a Claude entry declares no default effort, so an unsupported one
//! is dropped, and so is any effort sent with a model the Claude list does not carry.

use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;

use crate::actor::Actor;
use crate::auth::Principal;
use crate::claude_planner::models::{ClaudeCatalog, ClaudeModel};
use crate::db::sqlite::card_update_tx;
use crate::db::write_with_event_typed;
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::Event;
use crate::extract::{Json, JsonBody, Path};
use crate::ids::CardId;
use crate::model::CardPatch;
use crate::operation::codex_adapter::card_payload_get_tx;
use crate::planner_model::CardModelSelection;
use crate::routes::cards::card_scope;
use crate::routes::models::CODEX_READ_TIMEOUT;
use crate::routes::planner_cards::card_runs_headless_harness;
use crate::routes::track_report_blocks::require_rest_user_actor_for;
use crate::session_projection_repo::AgentProvider;
use crate::state::{CodexShellState, RouteState, WorkerState};

const ACTOR_SUBJECT: &str = "planner model selection";
const ACTOR_REDIRECT: &str = "Which model a person's conversation runs with is their own choice; agents have no write \
     path to it.";

/// Accept `null`, refuse absence: a `deserialize_with` makes the `Option<T>` field
/// required to serde while still accepting an explicit `null`.
fn required_nullable<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetPlannerModelBody {
    /// The model **slug** to run with (never a catalog entry's `id`; for a Claude Planner a
    /// `model` of its catalog, the Claude CLI's `value`), or `null` to follow the installation
    /// default (for a Claude Planner, the Claude CLI's own). `#[schema(required = true)]` is needed because utoipa
    /// derives optionality from the `Option<T>` alone and cannot see `deserialize_with`.
    #[schema(required = true)]
    #[serde(deserialize_with = "required_nullable")]
    pub model: Option<String>,
    /// The reasoning effort, or `null` to follow the default. Required. A bare string:
    /// codex accepts any non-empty effort. For a Claude Planner, one the chosen entry declares
    /// (for a `null` model, the CLI's `default` entry); any other is dropped (`effort_adjusted`).
    #[schema(required = true)]
    #[serde(deserialize_with = "required_nullable")]
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SetPlannerModelResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    /// The stored slug, echoed rather than assumed.
    pub model: Option<String>,
    /// The stored effort. Differs from the request when `effort_adjusted`.
    pub reasoning_effort: Option<String>,
    /// The requested effort was not supported by the chosen model and was moved to that
    /// model's own default (for a Claude Planner, which declares none, dropped to `null`).
    pub effort_adjusted: bool,
    /// The slug is not in the catalog the provider currently reports. A hint, not a refusal;
    /// always `false` when the catalog could not be read.
    pub unknown_model: bool,
}

#[utoipa::path(
    put,
    path = "/api/cards/{id}/planner/model",
    tag = "cards",
    params(("id" = String, Path, description = "Planner card id")),
    request_body = SetPlannerModelBody,
    responses(
        (status = 200, description = "Selection stored. `effort_adjusted` and `unknown_model` report what the catalog said about it; neither is an error", body = SetPlannerModelResponse),
        (status = 400, description = "A Claude Planner card while Claude is not ready, with the `GET /api/agent-providers` reason; nothing is stored", body = ErrorBody),
        (status = 401, description = "Unauthenticated", body = ErrorBody),
        (status = 403, description = "Not `X-Calm-Actor: user`, or the card is not a planner codex card", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 422, description = "`model` or `reasoning_effort` is missing from the body, or an unknown key is present. Both keys are required; `null` is how the default is chosen", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn set_planner_model(
    State(s): State<RouteState>,
    State(codex): State<CodexShellState>,
    State(workers): State<WorkerState>,
    _principal: Principal,
    actor: Actor,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<SetPlannerModelBody>,
) -> Result<(StatusCode, Json<SetPlannerModelResponse>)> {
    // The actor check runs first, so an agent probing card ids learns nothing from the status.
    require_rest_user_actor_for(&actor, ACTOR_SUBJECT, ACTOR_REDIRECT)?;

    let card = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    let role = s
        .write
        .verify_role(&card.id)
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    if !card_runs_headless_harness(&card, role) {
        return Err(CalmError::Forbidden(format!(
            "card {id} is not a planner codex card",
        )));
    }
    let provider = crate::harness::profile::PlannerBinding::from_card(&card, role)
        .ok_or_else(|| CalmError::Forbidden("The card has no declared Planner backend".into()))?
        .provider;
    let claude = provider == AgentProvider::Claude;

    let SetPlannerModelBody {
        model,
        reasoning_effort,
    } = body;
    let advice = if provider == AgentProvider::OpenCode {
        s.acp_planner.configured(&provider)?;
        catalog_advice(
            CatalogSource::Acp,
            model.as_deref(),
            reasoning_effort.as_deref(),
        )
        .await
    } else if claude {
        // #1822 6′: a Claude write needs Claude ready, as its create does (#1817), so the list is
        // in hand: an effort is judged against the chosen entry, and dropped for a model the list
        // does not carry (the CLI would ignore it). Codex is not asked.
        let catalog = s
            .provider_availability
            .claude(crate::agent_providers::Freshness::Cached, &s.claude_planner)
            .await
            .catalog()
            .map_err(|refusal| {
                CalmError::BadRequest(format!("card {id}: `planner_provider` {refusal}"))
            })?;
        catalog_advice(
            CatalogSource::Claude(&catalog),
            model.as_deref(),
            reasoning_effort.as_deref(),
        )
        .await
    } else {
        catalog_advice(
            CatalogSource::Codex(&codex),
            model.as_deref(),
            reasoning_effort.as_deref(),
        )
        .await
    };
    let stored_effort = match &advice.adjustment {
        Some(adjustment) => adjustment.to.clone(),
        None => reasoning_effort.clone(),
    };

    let scope = card_scope(s.repo.as_ref(), card.id.clone(), card.track_id.clone()).await?;
    let card_id = card.id.clone();
    let (_card, _event_id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.write,
        {
            let model = model.clone();
            let stored_effort = stored_effort.clone();
            let card_id = card_id.to_string();
            move |tx| {
                Box::pin(async move {
                    // Read-modify-write inside the transaction: the payload column is replaced wholesale.
                    let mut payload = card_payload_get_tx(tx, &card_id).await?;
                    let map = payload.as_object_mut().ok_or_else(|| {
                        CalmError::Internal(format!(
                            "planner card {card_id} payload is not a JSON object"
                        ))
                    })?;
                    CardModelSelection::apply_to_payload(
                        map,
                        model.as_deref(),
                        stored_effort.as_deref(),
                    );
                    let card = card_update_tx(
                        tx,
                        &card_id,
                        CardPatch {
                            title: None,
                            kind: None,
                            sort: None,
                            payload: Some(payload),
                            deletable: None,
                        },
                    )
                    .await?;
                    Ok((card.clone(), Event::CardUpdated(card)))
                })
            }
        },
    )
    .await?;

    // A model was just picked, so a harness waiting out its refusal interval must stop
    // waiting. Best-effort: no live harness means nothing was waiting.
    if let Ok(Some(runtime)) = s
        .repo
        .session_projection_active_for_card(&card_id.to_string())
        .await
        && let Some(harness) = workers.harness.get(&runtime.id)
    {
        harness.retry_issuance_now().await;
    }

    tracing::info!(
        actor = %actor.as_str(),
        card_id = %card_id,
        model = ?model,
        reasoning_effort = ?stored_effort,
        effort_adjusted = advice.adjustment.is_some(),
        unknown_model = advice.unknown_model,
        "planner conversation model selection stored"
    );

    Ok((
        StatusCode::OK,
        Json(SetPlannerModelResponse {
            card_id,
            model,
            reasoning_effort: stored_effort,
            effort_adjusted: advice.adjustment.is_some(),
            unknown_model: advice.unknown_model,
        }),
    ))
}

/// What the catalog says about a requested selection.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct CatalogAdvice {
    /// `Some` when the requested effort is not one the chosen entry supports.
    pub(super) adjustment: Option<EffortAdjustment>,
    pub(super) unknown_model: bool,
}

/// Where an unsupported effort is moved: the entry's own default, `None` for a provider that
/// declares none (every Claude entry), which drops the effort.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct EffortAdjustment {
    pub(super) to: Option<String>,
}

/// Where a write's catalog comes from; the advice itself is the same for both.
pub(super) enum CatalogSource<'a> {
    /// ACP choices are checked against the agent's fresh setup metadata at issuance.
    Acp,
    /// Codex's `model/list`, asked now. A catalog that cannot be read advises nothing: the
    /// selection is stored unjudged (#293).
    Codex(&'a CodexShellState),
    /// The Claude CLI's list, as the ready availability check cached it (#1822).
    Claude(&'a ClaudeCatalog),
}

/// One catalog entry as the advice judges it.
struct AdviceEntry<'a> {
    model: &'a str,
    efforts: Vec<&'a str>,
    default_effort: Option<&'a str>,
}

/// Ask `source`'s catalog about the requested pair. Every failure to ask Codex yields
/// [`CatalogAdvice::default`] — no adjustment, not unknown.
pub(super) async fn catalog_advice(
    source: CatalogSource<'_>,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
) -> CatalogAdvice {
    match source {
        CatalogSource::Acp => CatalogAdvice::default(),
        CatalogSource::Codex(codex) => {
            // With no model chosen there is no catalog entry to judge against.
            let Some(model) = model else {
                return CatalogAdvice::default();
            };
            let deadline = tokio::time::Instant::now() + CODEX_READ_TIMEOUT;
            let models = match codex.shared_codex_appserver.model_list(deadline).await {
                Ok(models) => models,
                Err(e) => {
                    tracing::warn!(error = %e, "planner model selection: model/list unavailable");
                    return CatalogAdvice::default();
                }
            };
            let entries: Vec<AdviceEntry<'_>> = models
                .iter()
                .map(|m| AdviceEntry {
                    model: &m.model,
                    efforts: m
                        .supported_reasoning_efforts
                        .iter()
                        .map(|o| o.reasoning_effort.as_str())
                        .collect(),
                    default_effort: Some(&m.default_reasoning_effort),
                })
                .collect();
            advise(&entries, None, Some(model), reasoning_effort)
        }
        CatalogSource::Claude(catalog) => {
            let entries: Vec<AdviceEntry<'_>> = catalog.models.iter().map(claude_entry).collect();
            // A `null` model runs the CLI's `default` entry, so its effort is judged there.
            let default = claude_entry(&catalog.default);
            let advice = advise(&entries, Some(&default), model, reasoning_effort);
            // The CLI does not judge an effort, and a model the list does not carry has no entry
            // to judge it against: the effort is dropped. (Codex keeps it: codex judges it.)
            if advice.unknown_model && reasoning_effort.is_some() {
                CatalogAdvice {
                    adjustment: Some(EffortAdjustment { to: None }),
                    unknown_model: true,
                }
            } else {
                advice
            }
        }
    }
}

fn claude_entry(m: &ClaudeModel) -> AdviceEntry<'_> {
    AdviceEntry {
        model: &m.value,
        efforts: m.effort_levels.iter().map(String::as_str).collect(),
        default_effort: None,
    }
}

/// The advice of `entries` (and `null_model`, the entry a `null` model runs where the provider
/// lists one) about the pair: an unlisted model is unknown, an unsupported effort moves to the
/// entry's default.
fn advise(
    entries: &[AdviceEntry<'_>],
    null_model: Option<&AdviceEntry<'_>>,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
) -> CatalogAdvice {
    let entry = match model {
        None => match null_model {
            Some(entry) => entry,
            None => return CatalogAdvice::default(),
        },
        Some(model) => match entries.iter().find(|e| e.model == model) {
            Some(entry) => entry,
            None => {
                return CatalogAdvice {
                    adjustment: None,
                    unknown_model: true,
                };
            }
        },
    };
    let Some(requested) = reasoning_effort else {
        return CatalogAdvice::default();
    };
    let supported = entry.efforts.contains(&requested);
    CatalogAdvice {
        adjustment: (!supported).then(|| EffortAdjustment {
            to: entry.default_effort.map(str::to_string),
        }),
        unknown_model: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry<'a>(
        model: &'a str,
        efforts: &[&'a str],
        default_effort: Option<&'a str>,
    ) -> AdviceEntry<'a> {
        AdviceEntry {
            model,
            efforts: efforts.to_vec(),
            default_effort,
        }
    }

    fn adjusted(to: Option<&str>) -> CatalogAdvice {
        CatalogAdvice {
            adjustment: Some(EffortAdjustment {
                to: to.map(str::to_string),
            }),
            unknown_model: false,
        }
    }

    #[test]
    fn codex_entries_move_an_unsupported_effort_to_the_entry_default() {
        let entries = [entry("gpt-5", &["low", "high"], Some("low"))];
        assert_eq!(
            advise(&entries, None, Some("gpt-5"), Some("high")),
            CatalogAdvice::default()
        );
        assert_eq!(
            advise(&entries, None, Some("gpt-5"), Some("max")),
            adjusted(Some("low"))
        );
        assert_eq!(
            advise(&entries, None, Some("gpt-6"), Some("max")),
            CatalogAdvice {
                adjustment: None,
                unknown_model: true
            }
        );
        // No entry is named for a null model on a provider without a default entry.
        assert_eq!(
            advise(&entries, None, None, Some("max")),
            CatalogAdvice::default()
        );
    }

    #[test]
    fn claude_entries_drop_an_unsupported_effort_and_judge_a_null_model_on_the_default_entry() {
        let entries = [
            entry("claude-fable-5-1[1m]", &["low", "max"], None),
            entry("haiku", &[], None),
        ];
        let default = entry("default", &["low", "high"], None);
        let judge = |model, effort| advise(&entries, Some(&default), model, effort);
        assert_eq!(
            judge(Some("claude-fable-5-1[1m]"), Some("max")),
            CatalogAdvice::default()
        );
        assert_eq!(
            judge(Some("claude-fable-5-1[1m]"), Some("high")),
            adjusted(None)
        );
        assert_eq!(judge(Some("haiku"), Some("low")), adjusted(None));
        assert_eq!(judge(None, Some("high")), CatalogAdvice::default());
        assert_eq!(judge(None, Some("max")), adjusted(None));
        assert_eq!(
            judge(Some("opus"), Some("low")),
            CatalogAdvice {
                adjustment: None,
                unknown_model: true
            }
        );
    }
}
