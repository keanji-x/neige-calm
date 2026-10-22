//! Shared catalog selection policy for HTTP and delegated creation.
use super::catalog::CODEX_READ_TIMEOUT;
use crate::claude_planner::models::ClaudeCatalog;
use crate::error::{CalmError, Result};
use provider::claude::discovery::ClaudeModel;

/// What the catalog says about a requested selection.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct CatalogAdvice {
    /// `Some` when the requested effort is not one the chosen entry supports.
    pub(crate) adjustment: Option<EffortAdjustment>,
    pub(crate) unknown_model: bool,
    pub(crate) catalog_unavailable: bool,
}

impl CatalogAdvice {
    /// Declarations must name a model the provider actually reports. Ordinary creation retains
    /// its advisory unknown-model behavior and its outage-tolerant no-message creation.
    pub(crate) fn require_child_selection(
        self,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<()> {
        if let Some(model) = model
            && (self.unknown_model || self.catalog_unavailable)
        {
            return Err(CalmError::BadRequest(format!(
                "child_track.model: {} `{}`",
                if self.catalog_unavailable {
                    "catalog unavailable; cannot validate"
                } else {
                    "unknown model"
                },
                model
            )));
        }
        self.require_create_selection(model, effort)
    }

    pub(crate) fn require_create_selection(
        self,
        model: Option<&str>,
        reasoning_effort: Option<&str>,
    ) -> Result<()> {
        if let (Some(adjustment), Some(effort)) = (self.adjustment, reasoning_effort) {
            let named = model.map_or_else(
                || "the default model".to_string(),
                |m| format!("model `{m}`"),
            );
            let default = match adjustment.to {
                Some(d) => format!("its current catalog default is `{d}`"),
                None if self.unknown_model => "the catalog does not list it".to_string(),
                None => "it declares no default effort".to_string(),
            };
            return Err(CalmError::BadRequest(format!(
                "track create: reasoning_effort `{effort}` is unsupported for {named}; {default}. \
             Refresh the model list and choose a supported effort."
            )));
        }
        Ok(())
    }
}

/// Where an unsupported effort is moved: the entry's own default, `None` for a provider that
/// declares none (every Claude entry), which drops the effort.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct EffortAdjustment {
    pub(crate) to: Option<String>,
}

/// Where a write's catalog comes from; the advice itself is the same for both.
pub(crate) enum CatalogSource<'a> {
    Discovered(&'a super::catalog::ModelsResponse),
    /// Codex's `model/list`, asked now. A catalog that cannot be read advises nothing: the
    /// selection is stored unjudged (#293).
    Codex(&'a crate::shared_codex_appserver::SharedCodexAppServer),
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
pub(crate) async fn catalog_advice(
    source: CatalogSource<'_>,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
) -> CatalogAdvice {
    match source {
        CatalogSource::Discovered(catalog) => {
            let entries: Vec<AdviceEntry<'_>> = catalog
                .models
                .iter()
                .map(|m| AdviceEntry {
                    model: &m.model,
                    efforts: m
                        .supported_reasoning_efforts
                        .iter()
                        .map(|e| e.reasoning_effort.as_str())
                        .collect(),
                    default_effort: m.default_reasoning_effort.as_deref(),
                })
                .collect();
            let mut advice = advise(&entries, None, model, reasoning_effort);
            advice.catalog_unavailable = catalog.source == super::catalog::ModelSource::Unavailable;
            advice
        }
        CatalogSource::Codex(codex) => {
            // With no model chosen there is no catalog entry to judge against.
            let Some(model) = model else {
                return CatalogAdvice::default();
            };
            let deadline = tokio::time::Instant::now() + CODEX_READ_TIMEOUT;
            let models = match codex.model_list(deadline).await {
                Ok(models) => models,
                Err(e) => {
                    tracing::warn!(error = %e, "planner model selection: model/list unavailable");
                    return CatalogAdvice {
                        catalog_unavailable: true,
                        ..Default::default()
                    };
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
                    catalog_unavailable: false,
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
                    catalog_unavailable: false,
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
        catalog_unavailable: false,
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
            catalog_unavailable: false,
        }
    }

    #[test]
    fn child_track_config_unknown_model_is_strict_only_for_declarations() {
        let entries = [entry("known", &["high"], Some("high"))];
        assert!(
            advise(&entries, None, Some("unknown"), None)
                .require_create_selection(Some("unknown"), None)
                .is_ok()
        );
        assert!(
            advise(&entries, None, Some("unknown"), None)
                .require_child_selection(Some("unknown"), None)
                .is_err()
        );
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
                unknown_model: true,
                catalog_unavailable: false
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
                unknown_model: true,
                catalog_unavailable: false
            }
        );
    }
}
